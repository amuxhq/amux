//! PairingService: a peer's pairing stream to this host's window, carried
//! to the profile's core, which runs the exchange. Only callers that came
//! in over the pre-trust pairing transport reach it.

use std::pin::Pin;
use std::sync::Weak;

use futures_util::{Stream, StreamExt as _};
use tonic::Status;

use crate::HostId;
use crate::identity::DeviceIdentity;
use crate::runtime::ProfileRuntime;
use crate::transport::{BoxedGrpcAuth, BoxedGrpcConnectInfo, PreTrustPairingReachability};
use crate::trust::Reachability;

type PairStream = Pin<Box<dyn Stream<Item = Result<wire::PairResponse, Status>> + Send>>;

/// This host as a pairing presents it: its id and its identity key.
#[derive(Clone)]
pub(crate) struct LocalPairingIdentity {
    host_id: HostId,
    pubkey: Vec<u8>,
}

impl LocalPairingIdentity {
    pub(crate) fn new(host_id: HostId, pubkey: Vec<u8>) -> Self {
        Self { host_id, pubkey }
    }

    pub(crate) fn from_device_identity(identity: &DeviceIdentity) -> Self {
        Self::new(identity.host_id, identity.public_key().to_vec())
    }

    /// The identity a pairing seals, under `name`.
    pub(crate) fn named(&self, name: &str) -> wire::PairingIdentity {
        wire::PairingIdentity {
            host_id: self.host_id.as_bytes().to_vec(),
            pubkey: self.pubkey.clone(),
            name: name.to_owned(),
            expires_at_ms: 0,
        }
    }
}

#[derive(Clone)]
pub(crate) struct PairingService {
    runtime: Weak<ProfileRuntime>,
    local_identity: LocalPairingIdentity,
    /// Read at each pairing: the name may change while the host runs.
    local_host: crate::routing::LiveLocalHost,
}

impl PairingService {
    pub(crate) fn new(
        runtime: Weak<ProfileRuntime>,
        local_identity: LocalPairingIdentity,
        local_host: crate::routing::LiveLocalHost,
    ) -> Self {
        Self {
            runtime,
            local_identity,
            local_host,
        }
    }
}

#[tonic::async_trait]
impl wire::pairing_service_server::PairingService for PairingService {
    type PairStream = PairStream;

    async fn pair(
        &self,
        request: tonic::Request<tonic::Streaming<wire::PairRequest>>,
    ) -> Result<tonic::Response<Self::PairStream>, Status> {
        let reachability = pairing_request_reachability(&request)?;
        let Some(runtime) = self.runtime.upgrade() else {
            return Err(Status::failed_precondition("profile is unavailable"));
        };
        let replies = runtime
            .driver()
            .respond(
                self.local_identity.named(&self.local_host.name()),
                reachability,
                request.into_inner(),
            )
            .await;
        let replies = replies.map(|reply| {
            reply.map(|message| wire::PairResponse {
                message: Some(message),
            })
        });
        Ok(tonic::Response::new(replies.boxed()))
    }
}

fn pairing_request_reachability<T>(
    request: &tonic::Request<T>,
) -> Result<Option<Reachability>, Status> {
    request
        .extensions()
        .get::<BoxedGrpcConnectInfo>()
        .and_then(|info| match &info.auth {
            BoxedGrpcAuth::PreTrustPairing { reachability } => Some(match reachability {
                PreTrustPairingReachability::Cloud => Some(Reachability::Cloud),
                PreTrustPairingReachability::Forwarded => None,
                PreTrustPairingReachability::Direct(addr) => {
                    Some(Reachability::Direct { addrs: vec![*addr] })
                }
            }),
            BoxedGrpcAuth::Trusted { .. } => None,
        })
        .ok_or_else(|| {
            Status::permission_denied("pairing RPC requires pre-trust pairing transport")
        })
}
