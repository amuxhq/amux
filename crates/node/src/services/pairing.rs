//! PairingService: a peer's pairing stream to this host's window, carried
//! to the profile's core, which runs the exchange. Only callers that came
//! in over the pre-trust pairing transport reach it.

use std::pin::Pin;
use std::sync::Weak;

use futures_util::{Stream, StreamExt as _};
use tonic::Status;

use crate::identity::DeviceIdentity;
use crate::runtime::ProfileRuntime;
use crate::transport::{BoxedGrpcAuth, BoxedGrpcConnectInfo, PreTrustPairingReachability};
use crate::trust::Reachability;
use crate::{HostId, audit};

const MAX_PAIRING_NAME_BYTES: usize = 256;

type PairStream = Pin<Box<dyn Stream<Item = Result<wire::pb::PairMessage, Status>> + Send>>;

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
    host_name: String,
}

impl PairingService {
    pub(crate) fn new(
        runtime: Weak<ProfileRuntime>,
        local_identity: LocalPairingIdentity,
        host_name: String,
    ) -> Self {
        Self {
            runtime,
            local_identity,
            host_name,
        }
    }
}

#[tonic::async_trait]
impl wire::pairing_service_server::PairingService for PairingService {
    type PairStream = PairStream;

    async fn pair(
        &self,
        request: tonic::Request<tonic::Streaming<wire::pb::PairMessage>>,
    ) -> Result<tonic::Response<Self::PairStream>, Status> {
        audit::pairing_start("spake2");
        let reachability = pairing_request_reachability(&request).inspect_err(|error| {
            audit::pairing_failure("spake2", error);
        })?;
        if self.host_name.len() > MAX_PAIRING_NAME_BYTES {
            return Err(Status::invalid_argument(
                "PROTOCOL_VIOLATION: name is too long",
            ));
        }
        let Some(runtime) = self.runtime.upgrade() else {
            return Err(Status::failed_precondition("profile is unavailable"));
        };
        let replies = runtime
            .driver()
            .respond(
                self.local_identity.named(&self.host_name),
                reachability,
                request.into_inner(),
            )
            .await;
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
                PreTrustPairingReachability::Direct(addr) => {
                    Some(Reachability::Direct { addrs: vec![*addr] })
                }
                PreTrustPairingReachability::NoReusableReachability => None,
            }),
            BoxedGrpcAuth::Trusted { .. } => None,
        })
        .ok_or_else(|| {
            Status::permission_denied("pairing RPC requires pre-trust pairing transport")
        })
}
