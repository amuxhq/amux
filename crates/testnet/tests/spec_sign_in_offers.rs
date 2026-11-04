//! One-tap sign-in between paired hosts: a signed-in host offers a paired
//! one its account, the offer reaches the peer over their link, and the
//! peer's person accepts or dismisses it. Accepting signs the peer in to the
//! same account with tokens of its own, through a one-time ticket the
//! stand-in account service binds to the peer's host id; dismissing ends
//! the offer. A peer already signed in is offered nothing.

#![cfg(unix)]

use std::time::Duration;

use support::*;
use testnet::{Net, PATIENCE, Relay, Topology, until};
use wire::{AnswerSignInOfferRequest, BindProfileRequest, Empty, OfferSignInRequest};

use crate::support;

const SCOPE: &str = "one-tap";

/// How long a story waits to see that nothing arrives.
const QUIET: Duration = Duration::from_secs(1);

/// Desk and laptop on one network, paired, with Ada's and Bob's accounts
/// at the stand-in account service; nobody signed in.
async fn paired() -> Net {
    let net = Net::start(
        Topology::new()
            .relay(&["ada", "bob"])
            .host_decl(lan_host("desk", SCOPE))
            .host_decl(lan_host("laptop", SCOPE)),
    )
    .await
    .unwrap();
    pair(&net, "laptop", "desk").await;
    net
}

/// Signs `host` in to `account` the way a browser sign-in does.
async fn sign_in(net: &Net, host: &str, account: &str) {
    door(net, host)
        .await
        .bind_profile(BindProfileRequest {
            profile_id: Some(profile(net, host)),
            cloud_url: net.relay().unwrap().url().to_owned(),
            staged_refresh_token: Relay::login(account),
            // It is paired already, as a first run's sign-in finds it.
            adopt_non_pristine: true,
            ..BindProfileRequest::default()
        })
        .await
        .expect("the profile binds to the account");
}

async fn profile_info(net: &Net, host: &str) -> wire::Profile {
    let id = profile(net, host);
    door(net, host)
        .await
        .list_profiles(Empty {})
        .await
        .unwrap()
        .into_inner()
        .profiles
        .into_iter()
        .find(|listed| listed.id == id)
        .expect("the profile is listed")
}

fn signed_in(profile: &wire::Profile) -> bool {
    matches!(
        profile
            .cloud
            .as_ref()
            .and_then(|cloud| cloud.standing.as_ref()),
        Some(wire::cloud_standing::Standing::SignedIn(_))
    )
}

/// `host` offers `peer` its account.
async fn offer(net: &Net, host: &str, peer: &str) {
    admin(net, host)
        .await
        .offer_sign_in(OfferSignInRequest {
            host_id: host_id(net, peer).as_bytes().to_vec(),
        })
        .await
        .expect("the offer stands");
}

/// The offers `host` is shown once there is one.
async fn offered(net: &Net, host: &str) -> Vec<wire::SignInOffer> {
    let mut offers = admin(net, host)
        .await
        .subscribe_sign_in_offers(Empty {})
        .await
        .unwrap()
        .into_inner();
    tokio::time::timeout(PATIENCE, async {
        loop {
            let now = offers
                .message()
                .await
                .unwrap()
                .expect("the stream stays up");
            if !now.offers.is_empty() {
                return now.offers;
            }
        }
    })
    .await
    .expect("an offer arrives")
}

/// The offers `host` is shown now.
async fn listed(net: &Net, host: &str) -> Vec<wire::SignInOffer> {
    admin(net, host)
        .await
        .subscribe_sign_in_offers(Empty {})
        .await
        .unwrap()
        .into_inner()
        .message()
        .await
        .unwrap()
        .expect("the stream starts with the offers now")
        .offers
}

async fn answer(
    net: &Net,
    host: &str,
    from: &str,
    accept: bool,
) -> Result<wire::Profile, tonic::Status> {
    admin(net, host)
        .await
        .answer_sign_in_offer(AnswerSignInOfferRequest {
            host_id: host_id(net, from).as_bytes().to_vec(),
            accept,
        })
        .await
        .map(tonic::Response::into_inner)
}

/// Desk is signed in to Ada and offers the laptop; the laptop is shown
/// Ada's email under the desk's name, accepts, and ends signed in to Ada
/// under its own client, through one ticket bound to its host id.
#[tokio::test(flavor = "multi_thread")]
async fn an_accepted_offer_signs_the_peer_in_to_the_same_account() {
    let net = paired().await;
    sign_in(&net, "desk", "ada").await;
    offer(&net, "desk", "laptop").await;

    let offers = offered(&net, "laptop").await;
    assert_eq!(offers.len(), 1);
    assert_eq!(offers[0].host_id, host_id(&net, "desk").as_bytes());
    assert_eq!(offers[0].host_name, "desk");
    assert_eq!(offers[0].email, "ada@example.com");
    assert_eq!(offers[0].account_name, "ada");

    let profile = answer(&net, "laptop", "desk", true)
        .await
        .expect("accepting signs the laptop in");
    assert!(signed_in(&profile), "{profile:?}");
    assert_eq!(profile.email, "ada@example.com");

    let relay = net.relay().unwrap();
    let tickets = relay.tickets();
    assert_eq!(tickets.len(), 1, "one ticket for one acceptance");
    assert_eq!(tickets[0].account, "ada");
    assert_eq!(
        tickets[0].host_id,
        host_id(&net, "laptop").simple().to_string(),
        "the ticket names the laptop"
    );
    assert!(tickets[0].spent);
    assert_eq!(
        relay.redeemed(),
        vec![("cli".to_owned(), true)],
        "the laptop redeemed it as the daemon's own client"
    );
    assert_eq!(
        relay.refreshed_as().last().map(String::as_str),
        Some("cli"),
        "the laptop's own refresh token, under its own client"
    );
    until("the laptop's relay link", || async {
        let links = relay.links("ada").await;
        (links.len() == 2)
            .then_some(())
            .ok_or_else(|| format!("ada's links {links:?}"))
    })
    .await
    .unwrap();
    assert!(
        admin(&net, "laptop")
            .await
            .subscribe_sign_in_offers(Empty {})
            .await
            .unwrap()
            .into_inner()
            .message()
            .await
            .unwrap()
            .unwrap()
            .offers
            .is_empty(),
        "an accepted offer is gone"
    );
    println!("the laptop accepted the desk's offer and is signed in to ada as cli");
    net.shutdown().await.unwrap();
}

/// A dismissed offer ends on both sides: the laptop's next link to the
/// desk brings no offer again.
#[tokio::test(flavor = "multi_thread")]
async fn a_dismissed_offer_ends_and_is_not_sent_again() {
    let mut net = paired().await;
    sign_in(&net, "desk", "ada").await;
    offer(&net, "desk", "laptop").await;
    offered(&net, "laptop").await;

    let profile = answer(&net, "laptop", "desk", false)
        .await
        .expect("dismissing answers the profile");
    assert!(!signed_in(&profile));

    net.stop_daemon("laptop").await.unwrap();
    net.restart_daemon("laptop").await.unwrap();
    until_via(&net, "laptop", "desk", node::harness::LiveRoute::Direct).await;
    let mut offers = admin(&net, "laptop")
        .await
        .subscribe_sign_in_offers(Empty {})
        .await
        .unwrap()
        .into_inner();
    assert!(offers.message().await.unwrap().unwrap().offers.is_empty());
    assert!(
        tokio::time::timeout(QUIET, offers.message()).await.is_err(),
        "no offer after the dismissal"
    );
    assert!(net.relay().unwrap().tickets().is_empty(), "nothing minted");
    assert!(!signed_in(&profile_info(&net, "laptop").await));
    println!("a dismissed offer stayed dismissed across the laptop's restart");
    net.shutdown().await.unwrap();
}

/// A dismissal made while the desk is out of reach is not undone when the
/// desk comes back: the desk sends its standing offer on the new link as
/// the laptop's answer travels the other way, and the laptop answers it by
/// dismissing again rather than showing it.
#[tokio::test(flavor = "multi_thread")]
async fn a_dismissal_made_offline_holds_when_the_offerer_returns() {
    // Linked in process, so the link can be cut and restored with neither
    // daemon restarting: the desk's offer stands in memory throughout.
    let mut net = Net::start(
        Topology::new()
            .relay(&["ada"])
            .host("desk")
            .host("laptop")
            .link("desk", "laptop"),
    )
    .await
    .unwrap();
    sign_in(&net, "desk", "ada").await;
    offer(&net, "desk", "laptop").await;
    offered(&net, "laptop").await;

    net.sever_link("desk", "laptop").unwrap();
    net.wait_link("desk", "laptop", false).await.unwrap();
    answer(&net, "laptop", "desk", false)
        .await
        .expect("dismissing offline answers the profile");
    net.restore_link("desk", "laptop").await.unwrap();
    net.wait_link("desk", "laptop", true).await.unwrap();

    let mut offers = admin(&net, "laptop")
        .await
        .subscribe_sign_in_offers(Empty {})
        .await
        .unwrap()
        .into_inner();
    assert!(offers.message().await.unwrap().unwrap().offers.is_empty());
    assert!(
        tokio::time::timeout(QUIET, offers.message()).await.is_err(),
        "the desk's offer on the new link is not shown"
    );
    println!("a dismissal made while the desk was away held when it came back");
    net.shutdown().await.unwrap();
}

/// An acceptance the account service turns down leaves the offer standing
/// on both hosts: the laptop is told, is still shown the offer, and
/// accepting it again once the service mints signs it in.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_acceptance_leaves_the_offer_to_accept_again() {
    let net = paired().await;
    sign_in(&net, "desk", "ada").await;
    offer(&net, "desk", "laptop").await;
    offered(&net, "laptop").await;
    let relay = net.relay().unwrap();

    relay.refuse_tickets(true);
    let refused = answer(&net, "laptop", "desk", true)
        .await
        .expect_err("no ticket, no sign-in");
    assert_eq!(refused.code(), tonic::Code::Unavailable, "{refused:?}");
    assert_eq!(
        listed(&net, "laptop").await.len(),
        1,
        "the offer still stands"
    );
    assert!(!signed_in(&profile_info(&net, "laptop").await));

    relay.refuse_tickets(false);
    let profile = answer(&net, "laptop", "desk", true)
        .await
        .expect("accepting again signs the laptop in");
    assert!(signed_in(&profile), "{profile:?}");
    assert!(listed(&net, "laptop").await.is_empty());
    println!(
        "an acceptance the service turned down ({}) was accepted again",
        refused.message()
    );
    net.shutdown().await.unwrap();
}

/// A ticket minted for another host is refused by the account service, and
/// the laptop stays signed out, still offered the account: accepting again
/// mints a fresh ticket, which signs it in.
#[tokio::test(flavor = "multi_thread")]
async fn a_ticket_for_another_host_signs_nobody_in() {
    let net = paired().await;
    sign_in(&net, "desk", "ada").await;
    let relay = net.relay().unwrap();
    relay.mint_tickets_for(Some(&host_id(&net, "desk").simple().to_string()));
    offer(&net, "desk", "laptop").await;
    offered(&net, "laptop").await;

    let refused = answer(&net, "laptop", "desk", true)
        .await
        .expect_err("another host's ticket signs nothing in");
    assert_eq!(refused.code(), tonic::Code::PermissionDenied, "{refused:?}");
    assert_eq!(relay.redeemed(), vec![("cli".to_owned(), false)]);
    assert!(!signed_in(&profile_info(&net, "laptop").await));
    assert_eq!(
        listed(&net, "laptop").await.len(),
        1,
        "the offer still stands"
    );

    relay.mint_tickets_for(None);
    let profile = answer(&net, "laptop", "desk", true)
        .await
        .expect("a ticket for the laptop signs it in");
    assert!(signed_in(&profile), "{profile:?}");
    assert_eq!(
        relay.tickets().len(),
        2,
        "a fresh ticket for the second try"
    );
    println!("another host's ticket was refused: {}", refused.message());
    net.shutdown().await.unwrap();
}

/// A laptop already signed in (to Bob) is never shown Ada's offer.
#[tokio::test(flavor = "multi_thread")]
async fn a_signed_in_peer_is_offered_nothing() {
    let net = paired().await;
    sign_in(&net, "laptop", "bob").await;
    sign_in(&net, "desk", "ada").await;
    offer(&net, "desk", "laptop").await;

    let mut offers = admin(&net, "laptop")
        .await
        .subscribe_sign_in_offers(Empty {})
        .await
        .unwrap()
        .into_inner();
    assert!(offers.message().await.unwrap().unwrap().offers.is_empty());
    assert!(
        tokio::time::timeout(QUIET, offers.message()).await.is_err(),
        "no offer to a signed-in laptop"
    );
    let refused = answer(&net, "laptop", "desk", true)
        .await
        .expect_err("there is nothing to accept");
    assert_eq!(
        refused.code(),
        tonic::Code::FailedPrecondition,
        "{refused:?}"
    );
    assert!(net.relay().unwrap().tickets().is_empty());
    println!("a signed-in laptop was offered nothing");
    net.shutdown().await.unwrap();
}

/// Only a signed-in profile offers, and only a paired host.
#[tokio::test(flavor = "multi_thread")]
async fn an_offer_needs_a_signed_in_profile_and_a_paired_peer() {
    let net = paired().await;
    let signed_out = admin(&net, "desk")
        .await
        .offer_sign_in(OfferSignInRequest {
            host_id: host_id(&net, "laptop").as_bytes().to_vec(),
        })
        .await
        .expect_err("a signed-out desk has no account to offer");
    assert_eq!(signed_out.code(), tonic::Code::FailedPrecondition);
    sign_in(&net, "desk", "ada").await;
    let stranger = admin(&net, "desk")
        .await
        .offer_sign_in(OfferSignInRequest {
            host_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
        })
        .await
        .expect_err("only a paired host is offered");
    assert_eq!(stranger.code(), tonic::Code::NotFound);
    net.shutdown().await.unwrap();
}

/// A signed-in profile hands out a one-time link that signs a browser in
/// and opens a page of the account service; a path off the service is
/// refused.
#[tokio::test(flavor = "multi_thread")]
async fn a_web_sign_in_link_opens_the_account_services_page() {
    let net = paired().await;
    let link = |path: &str| {
        let net = &net;
        let path = path.to_owned();
        async move {
            door(net, "desk")
                .await
                .web_sign_in_link(wire::WebSignInLinkRequest {
                    profile_id: profile(net, "desk"),
                    path,
                })
                .await
                .map(|answer| answer.into_inner().url)
        }
    };
    let signed_out = link("/offer")
        .await
        .expect_err("a signed-out desk has none");
    assert_eq!(signed_out.code(), tonic::Code::FailedPrecondition);
    sign_in(&net, "desk", "ada").await;
    let url = link("/offer").await.unwrap();
    let relay = net.relay().unwrap();
    assert!(url.starts_with(relay.url()), "{url}");
    assert!(url.ends_with("next=/offer"), "{url}");
    assert_eq!(
        relay.web_links(),
        vec![("ada".to_owned(), "/offer".to_owned())]
    );
    for elsewhere in ["https://example.com/offer", "//example.com/offer", "offer"] {
        let refused = link(elsewhere).await.expect_err("a path off the service");
        assert_eq!(refused.code(), tonic::Code::InvalidArgument, "{elsewhere}");
    }
    net.shutdown().await.unwrap();
}
