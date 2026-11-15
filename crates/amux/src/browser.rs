//! `amux internal browser`: the browser extension's way to amux.
//!
//! A browser starts this command itself, through native messaging, when the
//! extension connects, and talks to it over stdin and stdout: each message is
//! a 4-byte length in native byte order and that many bytes of JSON. The
//! command forwards each call the extension makes, as raw gRPC, to the
//! installation's front door or to one profile's client socket, and streams
//! the answers back. It never decodes a request or a response: the extension
//! encodes and decodes `amux.v1` itself, so this command has no per-call code
//! and the extension and this build meet only at `amux.v1`, which only adds.
//!
//! The extension sends
//!
//! ```text
//! {"id": 1, "call": {"target": "installation" | "<profile id, hex>", "method": "/amux.v1.…/…", "body": "<base64>"}}
//! {"id": 1, "cancel": true}
//! ```
//!
//! and this command answers with
//!
//! ```text
//! {"hello": {"version": "0.9.1"}}                     once, first
//! {"id": 1, "message": "<base64>", "more": true}       a part of a response message
//! {"id": 1, "message": "<base64>"}                     a message, or its last part
//! {"id": 1, "end": {"code": 0, "message": "", "details": "<base64>"}}
//! ```
//!
//! A cancelled call ends with gRPC's "cancelled" (1).
//!
//! A browser refuses a message to the extension over 1 MB, so a response
//! message is sent in parts of at most [`PART`] bytes before encoding, and
//! the extension joins them. A unary call is a stream of one message. Every
//! call names this client as the browser ([`wire::SURFACE_HEADER`]), so the
//! daemon counts it as one. A browser's own arguments (the extension's
//! origin, and on Windows a window handle) are accepted and ignored.
//!
//! The framing is a promise: an installed host manifest calls this command
//! long after the build that wrote it, from an extension at another version.
//! Fields are only added.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::{Buf as _, BufMut as _};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::{Mutex, mpsc};
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};
use tonic::transport::Channel;

/// The largest piece of a response message sent in one frame, before
/// base64: a browser refuses a frame to the extension over 1 MB, and base64
/// makes 768 KiB into 1 MiB, so this leaves room for the frame around it.
pub const PART: usize = 512 * 1024;

/// The largest frame read from the extension. A browser sends at most
/// 64 MiB; an attachment is at most 64 MiB of its own bytes, which base64
/// and the JSON around it make larger, so the limit allows for both.
const MOST_IN: u32 = 96 * 1024 * 1024;

/// What the surface header names this client as.
const SURFACE: &str = "browser";

/// The front door's target name; any other target is a profile's id in hex.
const INSTALLATION: &str = "installation";

/// Where the sockets are.
#[derive(Clone, Debug)]
pub struct Sockets {
    pub front_door: PathBuf,
}

/// Runs the bridge on this process's stdin and stdout until the browser
/// closes stdin.
pub async fn run(sockets: Sockets) -> Result<()> {
    serve(tokio::io::stdin(), tokio::io::stdout(), sockets).await
}

#[derive(Debug, Deserialize)]
struct Incoming {
    id: u64,
    #[serde(default)]
    call: Option<Call>,
    #[serde(default)]
    cancel: bool,
}

#[derive(Debug, Deserialize)]
struct Call {
    target: String,
    method: String,
    #[serde(default)]
    body: String,
}

/// Serves the bridge over `input` and `output`.
pub async fn serve<R, W>(mut input: R, output: W, sockets: Sockets) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (frames, mut queued) = mpsc::unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        let mut output = output;
        while let Some(frame) = queued.recv().await {
            if write_frame(&mut output, &frame).await.is_err() {
                break;
            }
        }
    });
    let _ = frames.send(json!({ "hello": { "version": node::version() } }));
    let routes = Arc::new(Routes::new(sockets));
    let mut calls: HashMap<u64, tokio::task::AbortHandle> = HashMap::new();
    loop {
        let frame = match read_frame(&mut input).await {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(error) => {
                eprintln!("amux internal browser: {error:#}");
                break;
            }
        };
        calls.retain(|_, call| !call.is_finished());
        let incoming: Incoming = match serde_json::from_slice(&frame) {
            Ok(incoming) => incoming,
            Err(error) => {
                eprintln!("amux internal browser: a frame that does not read: {error}");
                continue;
            }
        };
        let id = incoming.id;
        if incoming.cancel {
            if let Some(call) = calls.remove(&id) {
                call.abort();
                let _ = frames.send(end(id, &tonic::Status::cancelled("cancelled")));
            }
            continue;
        }
        let Some(call) = incoming.call else {
            continue;
        };
        let frames = frames.clone();
        let routes = routes.clone();
        let task = tokio::spawn(async move {
            let status = match forward(&routes, &call, id, &frames).await {
                Ok(()) => tonic::Status::ok(""),
                Err(status) => status,
            };
            let _ = frames.send(end(id, &status));
        });
        calls.insert(id, task.abort_handle());
    }
    for (_, call) in calls {
        call.abort();
    }
    drop(frames);
    let _ = writer.await;
    Ok(())
}

/// Makes one call and sends each response message as it arrives.
async fn forward(
    routes: &Routes,
    call: &Call,
    id: u64,
    frames: &mpsc::UnboundedSender<Value>,
) -> Result<(), tonic::Status> {
    let body = STANDARD
        .decode(&call.body)
        .map_err(|_| tonic::Status::invalid_argument("a call's body is not base64"))?;
    let path = call
        .method
        .parse::<tonic::codegen::http::uri::PathAndQuery>()
        .map_err(|_| tonic::Status::invalid_argument("a call's method is not a path"))?;
    let channel = routes.channel(&call.target).await?;
    // Sized as every other amux client is, so a blob up to the attachment
    // limit arrives (in parts) rather than failing tonic's 4 MiB default.
    let mut grpc = tonic::client::Grpc::new(channel)
        .max_decoding_message_size(wire::CHANNEL_MESSAGE_SIZE_LIMIT)
        .max_encoding_message_size(wire::CHANNEL_MESSAGE_SIZE_LIMIT);
    grpc.ready()
        .await
        .map_err(|error| tonic::Status::unavailable(error.to_string()))?;
    let mut request = tonic::Request::new(body);
    request.metadata_mut().insert(
        wire::SURFACE_HEADER,
        tonic::metadata::AsciiMetadataValue::from_static(SURFACE),
    );
    let mut stream = grpc
        .server_streaming(request, path, Raw)
        .await?
        .into_inner();
    while let Some(message) = stream.message().await? {
        for frame in parts(id, &message) {
            if frames.send(frame).is_err() {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// A response message as the frames that carry it.
pub fn parts(id: u64, message: &[u8]) -> Vec<Value> {
    let mut chunks = message.chunks(PART).peekable();
    if chunks.peek().is_none() {
        return vec![json!({ "id": id, "message": "" })];
    }
    let mut frames = Vec::new();
    while let Some(chunk) = chunks.next() {
        let mut frame = json!({ "id": id, "message": STANDARD.encode(chunk) });
        if chunks.peek().is_some() {
            frame["more"] = json!(true);
        }
        frames.push(frame);
    }
    frames
}

fn end(id: u64, status: &tonic::Status) -> Value {
    json!({
        "id": id,
        "end": {
            "code": status.code() as i32,
            "message": status.message(),
            "details": STANDARD.encode(status.details()),
        }
    })
}

/// The channels to the front door and each profile's socket, opened on
/// first use and kept.
struct Routes {
    sockets: Sockets,
    open: Mutex<HashMap<String, Channel>>,
}

impl Routes {
    fn new(sockets: Sockets) -> Routes {
        Routes {
            sockets,
            open: Mutex::new(HashMap::new()),
        }
    }

    async fn channel(&self, target: &str) -> Result<Channel, tonic::Status> {
        if let Some(channel) = self.open.lock().await.get(target) {
            return Ok(channel.clone());
        }
        let path = match target {
            INSTALLATION => self.sockets.front_door.clone(),
            profile => self.profile_socket(profile).await?,
        };
        self.dial(target, &path).await
    }

    async fn dial(&self, target: &str, path: &std::path::Path) -> Result<Channel, tonic::Status> {
        let channel = client::local_channel(path)
            .await
            .map_err(|error| tonic::Status::unavailable(error.to_string()))?;
        self.open
            .lock()
            .await
            .insert(target.to_owned(), channel.clone());
        Ok(channel)
    }

    /// A profile's client socket, as the front door names it.
    async fn profile_socket(&self, profile: &str) -> Result<PathBuf, tonic::Status> {
        use wire::installation_service_client::InstallationServiceClient;
        let open = self.open.lock().await.get(INSTALLATION).cloned();
        let door = match open {
            Some(door) => door,
            None => {
                let front_door = self.sockets.front_door.clone();
                self.dial(INSTALLATION, &front_door).await?
            }
        };
        let profiles = InstallationServiceClient::new(door)
            .list_profiles(wire::ListProfilesRequest {})
            .await?
            .into_inner()
            .profiles;
        profiles
            .into_iter()
            .find(|candidate| hex(&candidate.id) == profile)
            .map(|found| PathBuf::from(found.socket_path))
            .ok_or_else(|| tonic::Status::not_found(format!("no profile {profile}")))
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

async fn read_frame<R: AsyncRead + Unpin>(input: &mut R) -> Result<Option<Vec<u8>>> {
    let mut length = [0u8; 4];
    match input.read_exact(&mut length).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let length = u32::from_ne_bytes(length);
    anyhow::ensure!(length <= MOST_IN, "a frame of {length} bytes is too large");
    let mut frame = vec![0; length as usize];
    input
        .read_exact(&mut frame)
        .await
        .context("reading a frame")?;
    Ok(Some(frame))
}

async fn write_frame<W: AsyncWrite + Unpin>(output: &mut W, frame: &Value) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(frame).map_err(std::io::Error::other)?;
    let length = u32::try_from(bytes.len()).map_err(std::io::Error::other)?;
    output.write_all(&length.to_ne_bytes()).await?;
    output.write_all(&bytes).await?;
    output.flush().await
}

/// Messages as their encoded bytes, passed through untouched.
#[derive(Clone, Copy, Debug, Default)]
pub struct Raw;

impl Codec for Raw {
    type Encode = Vec<u8>;
    type Decode = Vec<u8>;
    type Encoder = Raw;
    type Decoder = Raw;

    fn encoder(&mut self) -> Self::Encoder {
        Raw
    }

    fn decoder(&mut self) -> Self::Decoder {
        Raw
    }
}

impl Encoder for Raw {
    type Item = Vec<u8>;
    type Error = tonic::Status;

    fn encode(&mut self, item: Vec<u8>, dst: &mut EncodeBuf<'_>) -> Result<(), tonic::Status> {
        dst.put_slice(&item);
        Ok(())
    }
}

impl Decoder for Raw {
    type Item = Vec<u8>;
    type Error = tonic::Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Vec<u8>>, tonic::Status> {
        Ok(Some(src.copy_to_bytes(src.remaining()).to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A message too large for one frame goes in parts the extension
    /// joins; an empty one is still a message.
    #[test]
    fn a_large_message_goes_in_parts() {
        let message: Vec<u8> = (0..PART * 2 + 10).map(|n| n as u8).collect();
        let frames = parts(7, &message);
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0]["more"], json!(true));
        assert_eq!(frames[1]["more"], json!(true));
        assert!(frames[2].get("more").is_none());
        let joined: Vec<u8> = frames
            .iter()
            .flat_map(|frame| STANDARD.decode(frame["message"].as_str().unwrap()).unwrap())
            .collect();
        assert_eq!(joined, message);
        for frame in &frames {
            assert!(serde_json::to_vec(frame).unwrap().len() < 1024 * 1024);
        }
        assert_eq!(parts(1, &[]), [json!({"id": 1, "message": ""})]);
    }

    /// Frames are a native-order length and that much JSON, both ways.
    #[tokio::test]
    async fn frames_round_trip() {
        let (mut ours, theirs) = tokio::io::duplex(1024);
        let (mut theirs_in, mut theirs_out) = tokio::io::split(theirs);
        write_frame(&mut ours, &json!({"id": 3, "cancel": true}))
            .await
            .unwrap();
        let frame = read_frame(&mut theirs_in).await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&frame).unwrap(),
            json!({"id": 3, "cancel": true})
        );
        theirs_out.write_all(&[1, 0]).await.unwrap();
        drop(theirs_out);
        drop(theirs_in);
        let mut ours = ours;
        assert!(read_frame(&mut ours).await.unwrap().is_none());
    }

    /// With nothing listening, a call ends unavailable rather than taking
    /// the bridge down, and the bridge greets first and stops when its
    /// input closes.
    #[tokio::test]
    async fn a_call_with_no_daemon_ends_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let (mut browser, bridge) = tokio::io::duplex(64 * 1024);
        let (bridge_in, bridge_out) = tokio::io::split(bridge);
        let sockets = Sockets {
            front_door: dir.path().join("amux.sock"),
        };
        let served = tokio::spawn(serve(bridge_in, bridge_out, sockets));
        let hello = read_frame(&mut browser).await.unwrap().unwrap();
        let hello: Value = serde_json::from_slice(&hello).unwrap();
        assert_eq!(hello["hello"]["version"], json!(node::version()));
        write_frame(
            &mut browser,
            &json!({"id": 1, "call": {"target": "installation", "method": "/amux.v1.InstallationService/ListProfiles", "body": ""}}),
        )
        .await
        .unwrap();
        let ended = read_frame(&mut browser).await.unwrap().unwrap();
        let ended: Value = serde_json::from_slice(&ended).unwrap();
        assert_eq!(ended["id"], json!(1));
        assert_eq!(ended["end"]["code"], json!(tonic::Code::Unavailable as i32));
        drop(browser);
        served.await.unwrap().unwrap();
    }
}
