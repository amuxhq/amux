//! One-tap sign-in between paired devices.
//!
//! The relay joins only devices on the same account, so a device paired
//! with a signed-in one is offered that account rather than made to sign
//! in again. The signed-in side's person consents for one peer
//! ([`Edge::offer_sign_in`]); that offer stands here, in memory, until the
//! peer dismisses it, a link from the peer says it is signed in, or the
//! peer is unpaired, and goes to the peer on each link it opens while its
//! handshake says it is signed out. The peer's person sees the account's
//! email and answers; accepting asks the offerer for a ticket, which the
//! offerer mints from the account service for the peer's host id and sends
//! back over the same encrypted link. The peer redeems it at once for its
//! own tokens under its own sign-in client and binds the profile the offer
//! reached.
//!
//! Accepting ends nothing by itself: the offer stands on both sides until
//! the peer is signed in, so an acceptance that fails anywhere along the
//! way (the account service, the link, the redemption) leaves the person
//! an offer to accept again.
//!
//! An offer is no token: it carries the account's name and where it lives,
//! and only the person accepting turns it into a ticket, which the account
//! service binds to the accepting host and honours once, within seconds.
//! Pairing is still the only trust decision; signing in adds a route.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::Stream;
use tokio::sync::{mpsc, oneshot, watch};
use tonic::Status;
use wire::message::Body;

use super::Edge;
use crate::HostId;

/// How long an accepted offer waits for the offerer's ticket: the offerer
/// asks the account service and answers over a live link, so this is a
/// slow network's answer, not a person's.
const TICKET_WAIT: Duration = Duration::from_secs(30);

/// How long the offerer waits on the account service for a ticket.
const MINT_TIMEOUT: Duration = Duration::from_secs(20);

/// The offers this profile made and was made, and the links they travel on.
pub(crate) struct SignInOffers {
    state: Mutex<State>,
    /// The offers paired peers made this profile, as clients are shown them.
    received: watch::Sender<Vec<wire::SignInOffer>>,
}

impl Default for SignInOffers {
    fn default() -> Self {
        SignInOffers {
            state: Mutex::default(),
            received: watch::Sender::new(Vec::new()),
        }
    }
}

#[derive(Default)]
struct State {
    /// The peers this profile's person offered the account to.
    standing: HashSet<HostId>,
    /// Each paired peer's live links: where a frame to it goes, and whether
    /// the link's handshake said the peer is signed in.
    links: HashMap<HostId, Vec<PeerLink>>,
    /// The offers peers made this profile, with the account service each
    /// names.
    received: BTreeMap<HostId, (wire::SignInOffer, String)>,
    /// The peers whose offer this profile's person dismissed. Their offers
    /// are answered with the dismissal again rather than shown: one that
    /// was standing when the dismissal could not be sent arrives on the
    /// next link, and one already in flight crosses the dismissal.
    dismissed: HashSet<HostId>,
    /// Acceptances waiting for the offering peer's ticket.
    awaiting: HashMap<HostId, oneshot::Sender<wire::SignInTicket>>,
}

struct PeerLink {
    out: mpsc::Sender<wire::Message>,
    signed_in: bool,
}

impl State {
    /// The peer's links still open.
    fn open(&mut self, peer: HostId) -> impl Iterator<Item = &PeerLink> {
        let links = self.links.entry(peer).or_default();
        links.retain(|link| !link.out.is_closed());
        links.iter()
    }

    /// Sends `body` on one of the peer's open links; false when none is up
    /// or it would not take the frame.
    fn send(&mut self, peer: HostId, body: Body) -> bool {
        let message = wire::Message { body: Some(body) };
        self.open(peer)
            .any(|link| link.out.try_send(message.clone()).is_ok())
    }
}

fn dismissal() -> Body {
    Body::SignInAnswer(wire::SignInOfferAnswer { accept: false })
}

fn ticket_error(code: wire::ErrorCode, message: &str) -> Body {
    Body::SignInTicket(wire::SignInTicket {
        outcome: Some(wire::sign_in_ticket::Outcome::Error(
            wire::google::rpc::Status {
                code: code as i32,
                message: message.to_owned(),
                details: Vec::new(),
            },
        )),
    })
}

impl SignInOffers {
    fn publish(&self, state: &State) {
        let offers = state
            .received
            .values()
            .map(|(offer, _)| offer.clone())
            .collect::<Vec<_>>();
        self.received.send_if_modified(|current| {
            let changed = *current != offers;
            *current = offers;
            changed
        });
    }
}

impl Edge {
    fn offers(&self) -> &SignInOffers {
        &self.sign_in_offers
    }

    /// The offer this profile's account makes a peer, when it is signed in.
    fn offer_frame(&self) -> Option<Body> {
        if !self.account.wants_cloud() {
            return None;
        }
        let record = self.account.record()?;
        Some(Body::SignInOffer(wire::SignInOfferMade {
            email: record.email.unwrap_or_default(),
            account_name: record.name.unwrap_or_default(),
            cloud_url: record.service.as_str().to_owned(),
        }))
    }

    /// The person consents to offering `host` this profile's account. The
    /// offer stands until the peer dismisses it, signs in or is unpaired,
    /// and is sent now on a link that says the peer is signed out.
    pub fn offer_sign_in(&self, host: HostId) -> Result<(), Status> {
        let Some(frame) = self.offer_frame() else {
            return Err(Status::failed_precondition(
                "sign this profile in before offering its account",
            ));
        };
        if !self.is_trusted(host) {
            return Err(Status::not_found(format!("peer {host} is not paired")));
        }
        let mut state = self.offers().state.lock().unwrap();
        let links = state
            .open(host)
            .map(|link| link.signed_in)
            .collect::<Vec<_>>();
        // A peer that says it is signed in is not offered another account.
        if links.iter().any(|signed_in| *signed_in) {
            state.standing.remove(&host);
            return Ok(());
        }
        state.standing.insert(host);
        state.send(host, frame);
        Ok(())
    }

    /// A link to the paired `peer` came up, its handshake saying whether
    /// the peer is signed in: a standing offer goes on it while the peer is
    /// signed out, and one the peer has signed in since is let go.
    pub(crate) fn sign_in_link_up(
        &self,
        peer: HostId,
        signed_in: bool,
        out: mpsc::Sender<wire::Message>,
    ) {
        let frame = self.offer_frame();
        let mut state = self.offers().state.lock().unwrap();
        state.open(peer).for_each(drop);
        state
            .links
            .entry(peer)
            .or_default()
            .push(PeerLink { out, signed_in });
        if signed_in {
            state.standing.remove(&peer);
        } else if state.standing.contains(&peer)
            && let Some(frame) = frame
        {
            state.send(peer, frame);
        }
    }

    /// A sign-in frame a paired peer sent on its link.
    pub(crate) fn sign_in_frame(self: &Arc<Self>, peer: HostId, body: Body) {
        match body {
            Body::SignInOffer(made) => self.offered(peer, made),
            Body::SignInAnswer(answer) => self.answered(peer, answer.accept),
            Body::SignInTicket(ticket) => {
                let waiter = self.offers().state.lock().unwrap().awaiting.remove(&peer);
                if let Some(waiter) = waiter {
                    let _ = waiter.send(ticket);
                }
            }
            _ => {}
        }
    }

    /// A peer offered this profile its account: shown to clients while this
    /// profile is signed out, unless its person dismissed that peer's offer.
    fn offered(&self, peer: HostId, made: wire::SignInOfferMade) {
        if self.account.wants_cloud() {
            return;
        }
        let Some(name) = self
            .trust
            .read()
            .ok()
            .and_then(|store| store.entry(peer).map(|entry| entry.name.clone()))
        else {
            return;
        };
        let offer = wire::SignInOffer {
            host_id: peer.as_bytes().to_vec(),
            host_name: name,
            email: made.email,
            account_name: made.account_name,
        };
        let offers = self.offers();
        let mut state = offers.state.lock().unwrap();
        if state.dismissed.contains(&peer) {
            state.send(peer, dismissal());
            return;
        }
        state.received.insert(peer, (offer, made.cloud_url));
        offers.publish(&state);
    }

    /// The peer answered this profile's offer: a dismissal ends it, an
    /// acceptance asks the account service for a ticket for the peer and
    /// leaves the offer standing, so a failure on the way can be answered
    /// by accepting again.
    fn answered(self: &Arc<Self>, peer: HostId, accept: bool) {
        let stood = {
            let mut state = self.offers().state.lock().unwrap();
            if accept {
                state.standing.contains(&peer)
            } else {
                state.standing.remove(&peer)
            }
        };
        if !stood {
            if accept {
                self.offers().state.lock().unwrap().send(
                    peer,
                    ticket_error(
                        wire::ErrorCode::FailedPrecondition,
                        "no offer of this account stands for this device",
                    ),
                );
            }
            return;
        }
        self.analytics.record(analytics::Event::SignInOffer {
            role: analytics::OfferRole::Offerer,
            outcome: if accept {
                analytics::OfferOutcome::Accepted
            } else {
                analytics::OfferOutcome::Dismissed
            },
        });
        if !accept {
            return;
        }
        let edge = self.clone();
        tokio::spawn(async move {
            let frame = match edge.mint_ticket(peer).await {
                Ok(ticket) => Body::SignInTicket(wire::SignInTicket {
                    outcome: Some(wire::sign_in_ticket::Outcome::Ticket(ticket)),
                }),
                Err(status) => {
                    tracing::warn!(%peer, error = %status.message(), "could not mint a sign-in ticket");
                    ticket_error(
                        wire::ErrorCode::try_from(status.code() as i32)
                            .unwrap_or(wire::ErrorCode::Unknown),
                        status.message(),
                    )
                }
            };
            edge.offers().state.lock().unwrap().send(peer, frame);
        });
    }

    /// A one-time ticket from the account service that signs `peer` in to
    /// this profile's account.
    async fn mint_ticket(&self, peer: HostId) -> Result<String, Status> {
        #[derive(serde::Deserialize)]
        struct Minted {
            ticket: String,
        }
        let service = self
            .account_service()
            .ok_or_else(|| Status::failed_precondition("this profile is signed out"))?;
        let token = self
            .access_token()
            .await
            .map_err(|error| Status::unavailable(error.to_string()))?;
        let answer = reqwest::Client::builder()
            .timeout(MINT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| Status::internal(error.to_string()))?
            .post(format!(
                "{}/api/sign-in-tickets",
                service.trim_end_matches('/')
            ))
            .bearer_auth(&token.bearer)
            .json(&serde_json::json!({ "host_id": peer.simple().to_string() }))
            .send()
            .await
            .map_err(|error| Status::unavailable(error.to_string()))?;
        let status = answer.status();
        if !status.is_success() {
            return Err(Status::unavailable(format!(
                "the account service answered {status} for a sign-in ticket"
            )));
        }
        answer
            .json::<Minted>()
            .await
            .map(|minted| minted.ticket)
            .map_err(|error| Status::unavailable(format!("an unreadable ticket: {error}")))
    }

    /// The offers paired peers made this profile, now and on every change.
    pub fn watch_sign_in_offers(&self) -> impl Stream<Item = wire::SignInOffers> + use<> {
        let watched = self.offers().received.subscribe();
        futures_util::stream::unfold((watched, true), |(mut watched, first)| async move {
            if !first && watched.changed().await.is_err() {
                return None;
            }
            let offers = watched.borrow_and_update().clone();
            Some((wire::SignInOffers { offers }, (watched, false)))
        })
    }

    /// The offers paired peers made this profile now.
    pub fn sign_in_offers(&self) -> Vec<wire::SignInOffer> {
        self.offers().received.borrow().clone()
    }

    /// The person turned `host`'s offer down: the offer ends there too, now
    /// if a link is up and otherwise when `host` next offers it.
    pub(crate) fn dismiss_sign_in_offer(&self, host: HostId) -> Result<(), Status> {
        let offers = self.offers();
        let mut state = offers.state.lock().unwrap();
        if state.received.remove(&host).is_none() {
            return Err(Status::not_found(format!(
                "{host} made this profile no offer"
            )));
        }
        offers.publish(&state);
        state.dismissed.insert(host);
        state.send(host, dismissal());
        drop(state);
        self.analytics.record(analytics::Event::SignInOffer {
            role: analytics::OfferRole::Receiver,
            outcome: analytics::OfferOutcome::Dismissed,
        });
        Ok(())
    }

    /// The person accepted `host`'s offer: the ticket the offerer minted for
    /// this host, and the account service to redeem it at. The offer stays
    /// listed until the profile is signed in, so a failure here or in the
    /// redemption leaves it to accept again; it goes only when the offerer
    /// says it no longer stands.
    pub(crate) async fn take_sign_in_ticket(
        &self,
        host: HostId,
    ) -> Result<(String, String), Status> {
        if self.account.wants_cloud() {
            return Err(Status::failed_precondition(
                "this profile is signed in already; sign it out to take another account",
            ));
        }
        let offers = self.offers();
        let (ticket, cloud_url) = {
            let mut state = offers.state.lock().unwrap();
            let Some((offer, cloud_url)) = state.received.get(&host).cloned() else {
                return Err(Status::not_found(format!(
                    "{host} made this profile no offer"
                )));
            };
            if state.awaiting.contains_key(&host) {
                return Err(Status::aborted(format!(
                    "{} is already being asked for a ticket",
                    offer.host_name
                )));
            }
            let (tx, rx) = oneshot::channel();
            state.awaiting.insert(host, tx);
            let asked = state.send(
                host,
                Body::SignInAnswer(wire::SignInOfferAnswer { accept: true }),
            );
            if !asked {
                state.awaiting.remove(&host);
                return Err(Status::unavailable(format!(
                    "{} is not connected; open amux there and try again",
                    offer.host_name
                )));
            }
            (rx, cloud_url)
        };
        self.analytics.record(analytics::Event::SignInOffer {
            role: analytics::OfferRole::Receiver,
            outcome: analytics::OfferOutcome::Accepted,
        });
        let answered = tokio::time::timeout(TICKET_WAIT, ticket).await;
        offers.state.lock().unwrap().awaiting.remove(&host);
        let ticket = match answered {
            Ok(Ok(ticket)) => ticket,
            Ok(Err(_)) => return Err(Status::unavailable("the offering device went away")),
            Err(_) => {
                return Err(Status::deadline_exceeded(
                    "the offering device sent no sign-in ticket",
                ));
            }
        };
        match ticket.outcome {
            Some(wire::sign_in_ticket::Outcome::Ticket(ticket)) => Ok((ticket, cloud_url)),
            Some(wire::sign_in_ticket::Outcome::Error(error)) => {
                let code = tonic::Code::from_i32(error.code);
                // The offerer keeps no offer for this host any more (it
                // restarted, or signed out): there is nothing to accept again.
                if code == tonic::Code::FailedPrecondition {
                    let mut state = offers.state.lock().unwrap();
                    state.received.remove(&host);
                    offers.publish(&state);
                }
                Err(Status::new(code, error.message))
            }
            None => Err(Status::unavailable(
                "the offering device sent an empty ticket",
            )),
        }
    }

    /// Forgets every offer to and from `host`: it is no longer paired.
    pub(crate) fn forget_sign_in_offers(&self, host: HostId) {
        let offers = self.offers();
        let mut state = offers.state.lock().unwrap();
        state.standing.remove(&host);
        state.received.remove(&host);
        state.dismissed.remove(&host);
        state.links.remove(&host);
        offers.publish(&state);
    }

    /// The profile signed in or out: offers made to it go once it is signed
    /// in, since only a signed-out profile takes one, and offers it made go
    /// once it is signed out, since it can no longer mint their tickets.
    pub(crate) fn account_moved_for_offers(&self) {
        let offers = self.offers();
        let mut state = offers.state.lock().unwrap();
        if self.account.wants_cloud() {
            state.received.clear();
            state.dismissed.clear();
        } else {
            state.standing.clear();
        }
        offers.publish(&state);
    }
}

/// The receiver's half once it has its ticket: redeems it under this
/// device's own sign-in client and binds the profile the offer reached.
pub(crate) async fn redeem_and_bind(
    registry: &crate::registry::Registry,
    profile: crate::ProfileId,
    edge: &Edge,
    ticket: String,
    cloud_url: String,
) -> Result<crate::registry::ProfileState, Status> {
    let host = edge.host_id().simple().to_string();
    let client = edge.sign_in_client.clone();
    let refresh_token =
        crate::auth::oauth::redeem_sign_in_ticket(&cloud_url, &client, &ticket, &host)
            .await
            .map_err(|error| Status::permission_denied(error.to_string()))?;
    registry
        .bind_via(
            crate::registry::SignIn {
                profile: Some(profile),
                cloud_url,
                refresh_token,
                client_id: client,
                // The offer reached this profile, which is paired with the
                // offerer: what it holds is adopted, not refused.
                adopt: true,
            },
            analytics::SignInVia::PairedDevice,
        )
        .await
        .map_err(Status::from)
}
