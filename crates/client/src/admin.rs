//! The profile's administration, answered by the client's own node only:
//! signing out, its identity, pairing, sign-in offers, dumps and reports.

use tonic::transport::Channel;
use wire::profile_admin_service_client::ProfileAdminServiceClient;

use crate::{Conn, EventStream, RpcError, Surface, events, held, held_events, stub};

#[derive(Clone)]
pub struct Admin {
    conn: Conn,
}

impl Admin {
    pub(crate) fn new(conn: Conn) -> Admin {
        Admin { conn }
    }

    /// The administration on the node's channel of a socket being
    /// connected, before the client exists.
    pub(crate) fn on(node: &Channel, surface: Surface) -> Admin {
        Admin::new(Conn::on_node(node.clone(), surface))
    }

    fn stub(&self) -> ProfileAdminServiceClient<Conn> {
        stub!(
            ProfileAdminServiceClient<Conn>,
            (self.conn.clone(), self.conn.origin())
        )
    }

    /// Writes a dump of the profile, the client's sessions as `request`
    /// carries them included.
    pub async fn dump(&self, request: wire::DumpRequest) -> Result<wire::DumpResponse, RpcError> {
        Ok(self.stub().dump(request).await?.into_inner())
    }

    /// Writes a person's report around a dump and sends it on when the
    /// profile is signed in.
    pub async fn submit_report(
        &self,
        request: wire::SubmitReportRequest,
    ) -> Result<wire::SubmitReportResponse, RpcError> {
        Ok(self.stub().submit_report(request).await?.into_inner())
    }

    /// Signs the profile out: its relay link goes down and its refresh
    /// token is forgotten, but it stays tied to its account.
    pub async fn sign_out(&self) -> Result<(), RpcError> {
        self.stub().sign_out(wire::SignOutRequest {}).await?;
        Ok(())
    }

    /// This profile's identity as its peers pin it: its host id among it.
    pub async fn get_device_identity(&self) -> Result<wire::DeviceIdentity, RpcError> {
        let request = wire::GetDeviceIdentityRequest {};
        held(
            self.stub()
                .get_device_identity(request)
                .await?
                .into_inner()
                .identity,
        )
    }

    /// Opens a pairing window on this profile.
    pub async fn start_pairing(
        &self,
        request: wire::StartPairingRequest,
    ) -> Result<wire::PairingWindow, RpcError> {
        held(
            self.stub()
                .start_pairing(request)
                .await?
                .into_inner()
                .window,
        )
    }

    /// Closes the profile's pairing window, if one is open.
    pub async fn cancel_pairing(&self) -> Result<(), RpcError> {
        self.stub()
            .cancel_pairing(wire::CancelPairingRequest {})
            .await?;
        Ok(())
    }

    /// The profile's pairing window, now and on every change.
    pub async fn subscribe_pairing(&self) -> Result<EventStream<wire::PairingStatus>, RpcError> {
        let stream = self
            .stub()
            .subscribe_pairing(wire::SubscribePairingRequest {})
            .await?
            .into_inner();
        Ok(held_events(stream, |response| response.status))
    }

    /// Offers the paired host `host_id` this profile's account.
    pub async fn offer_sign_in(&self, host_id: Vec<u8>) -> Result<(), RpcError> {
        self.stub()
            .offer_sign_in(wire::OfferSignInRequest { host_id })
            .await?;
        Ok(())
    }

    /// The offers paired hosts made this profile, now and on every change.
    pub async fn subscribe_sign_in_offers(
        &self,
    ) -> Result<EventStream<wire::SubscribeSignInOffersResponse>, RpcError> {
        let stream = self
            .stub()
            .subscribe_sign_in_offers(wire::SubscribeSignInOffersRequest {})
            .await?
            .into_inner();
        Ok(events(stream))
    }

    /// Accepts an offer, which signs this profile in, or dismisses it.
    pub async fn answer_sign_in_offer(
        &self,
        request: wire::AnswerSignInOfferRequest,
    ) -> Result<wire::Profile, RpcError> {
        held(
            self.stub()
                .answer_sign_in_offer(request)
                .await?
                .into_inner()
                .profile,
        )
    }
}
