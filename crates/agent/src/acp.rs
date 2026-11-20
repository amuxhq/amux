//! Hosting an agent that speaks the Agent Client Protocol: JSON-RPC over
//! the program's stdin and stdout, on every platform. The messages are
//! `acp_protocol` types, built here for the handshake and read back by the
//! interpreter; the process itself is started by the provider module, the
//! way headless Claude's is.
//!
//! amux offers the agent no files and no terminals, so the agent does its
//! own I/O; it does offer elicitation forms and links, which the
//! interpreter turns into asks.

use std::io;
use std::path::{Path, PathBuf};

use acp_protocol::client::{
    InitializeRequest, LoadSessionRequest, NewSessionRequest, PROTOCOL_VERSION,
    ResumeSessionRequest,
};
use acp_protocol::content::{
    BlobResourceContents, EmbeddedResource, EmbeddedResourceResource, ImageContent,
    TextResourceContents,
};
use acp_protocol::session::{
    ClientCapabilities, ElicitationCapabilities, Implementation, McpServer, McpServerStdio,
    PromptCapabilities,
};
use acp_protocol::{ClientMessage, ClientRequest, ContentBlock, Empty, Extra, RequestId};
use base64::Engine as _;
use interpret::acp::Opening;
use wire::Attachment;

/// What a prompt may carry beyond text, as the agent said at `initialize`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Takes {
    pub images: bool,
    /// Files' contents embedded in the prompt, rather than named for the
    /// agent to read.
    pub embedded: bool,
}

impl Takes {
    pub fn of(prompt: Option<&PromptCapabilities>) -> Self {
        Self {
            images: prompt.is_some_and(|prompt| prompt.image == Some(true)),
            embedded: prompt.is_some_and(|prompt| prompt.embedded_context == Some(true)),
        }
    }
}

/// The `initialize` request under `id`: ACP's version, amux by name, and
/// forms and links as the only things amux does for the agent.
pub fn initialize(id: &str) -> Vec<u8> {
    let request = InitializeRequest {
        protocol_version: PROTOCOL_VERSION,
        client_capabilities: Some(ClientCapabilities {
            elicitation: Some(Some(ElicitationCapabilities {
                form: Some(Some(Empty::default())),
                url: Some(Some(Empty::default())),
                ..Default::default()
            })),
            ..Default::default()
        }),
        client_info: Some(Some(Implementation {
            name: "amux".to_owned(),
            title: Some(Some("amux".to_owned())),
            version: crate::VERSION.to_owned(),
            meta: None,
            extra: Extra::new(),
        })),
        meta: None,
        extra: Extra::new(),
    };
    acp_protocol::encode(&ClientMessage::request(
        RequestId::String(id.to_owned()),
        ClientRequest::Initialize(request),
    ))
}

/// What a session is opened with besides its folder.
pub struct SessionSetup {
    pub cwd: String,
    /// More folders the session may work in; sent only to an agent that
    /// says it takes them.
    pub folders: Vec<String>,
    /// amux's own tool server, when the agent has an install path to run it
    /// from.
    pub tool_server: Option<(String, Vec<String>)>,
}

impl SessionSetup {
    fn servers(&self) -> Vec<McpServer> {
        self.tool_server
            .iter()
            .map(|(command, args)| {
                McpServer::Stdio(McpServerStdio {
                    name: interpret::AMUX_TOOL_SERVER.to_owned(),
                    command: command.clone(),
                    args: args.clone(),
                    env: Vec::new(),
                    meta: None,
                    extra: Extra::new(),
                })
            })
            .collect()
    }

    fn folders(&self, takes_folders: bool) -> Option<Vec<String>> {
        (takes_folders && !self.folders.is_empty()).then(|| self.folders.clone())
    }
}

/// The request that opens the session `opening` names, under the
/// interpreter's id for it; None for a conversation no new process can
/// reopen, which the interpreter ends.
pub fn open_session(
    opening: &Opening,
    setup: &SessionSetup,
    takes_folders: bool,
) -> Option<Vec<u8>> {
    use interpret::acp::{LOAD_REQUEST, NEW_SESSION_REQUEST, RESUME_REQUEST};
    let (id, request) = match opening {
        Opening::New => (
            NEW_SESSION_REQUEST,
            ClientRequest::SessionNew(NewSessionRequest {
                cwd: setup.cwd.clone(),
                additional_directories: setup.folders(takes_folders),
                mcp_servers: setup.servers(),
                system_prompt: None,
                meta: None,
                extra: Extra::new(),
            }),
        ),
        Opening::Resume(session) => (
            RESUME_REQUEST,
            ClientRequest::SessionResume(ResumeSessionRequest {
                session_id: session.clone(),
                cwd: setup.cwd.clone(),
                additional_directories: setup.folders(takes_folders),
                mcp_servers: Some(setup.servers()),
                system_prompt: None,
                meta: None,
                extra: Extra::new(),
            }),
        ),
        Opening::Load(session) => (
            LOAD_REQUEST,
            ClientRequest::SessionLoad(LoadSessionRequest {
                mcp_servers: setup.servers(),
                cwd: setup.cwd.clone(),
                additional_directories: setup.folders(takes_folders),
                session_id: session.clone(),
                system_prompt: None,
                meta: None,
                extra: Extra::new(),
            }),
        ),
        Opening::Unresumable => return None,
    };
    Some(acp_protocol::encode(&ClientMessage::request(
        RequestId::String(id.to_owned()),
        request,
    )))
}

/// A `session/prompt` request with its attachments put into its prompt.
/// The text carries each attachment's element at its place, which names
/// the file where the agent can open it itself; after the element come the
/// attachment's contents where the agent takes them: an image's bytes as
/// an image block, a file's as an embedded resource. What the agent does
/// not take reaches it as the element alone.
pub fn prompt_with_attachments(
    request: &[u8],
    attachments: &[Attachment],
    blobs: &Path,
    takes: Takes,
) -> io::Result<Vec<u8>> {
    let mut message = acp_protocol::decode_client(request)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let ClientMessage::Request {
        request: ClientRequest::SessionPrompt(prompt),
        ..
    } = &mut message
    else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "an ACP prompt effect that is not a session/prompt request",
        ));
    };
    let text: String = prompt
        .prompt
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect();
    prompt.prompt = blocks(&text, attachments, blobs, takes);
    Ok(acp_protocol::encode(&message))
}

fn blocks(text: &str, attachments: &[Attachment], blobs: &Path, takes: Takes) -> Vec<ContentBlock> {
    use attachments::Piece;
    use wire::attachment::Of;

    let positioned = attachments::Positioned {
        text: text.to_owned(),
        attachments: attachments.to_vec(),
    };
    let mut blocks = Vec::new();
    let mut prose = String::new();
    let flush = |prose: &mut String, blocks: &mut Vec<ContentBlock>| {
        if !prose.is_empty() {
            blocks.push(ContentBlock::text(std::mem::take(prose)));
        }
    };
    for piece in attachments::pieces(&positioned, blobs) {
        let (element, attachment) = match piece {
            Piece::Text(text) => {
                prose.push_str(text);
                continue;
            }
            Piece::Element {
                element,
                attachment,
                ..
            } => (element, attachment),
        };
        prose.push_str(&element);
        let mut sent_as_image = false;
        if takes.images {
            for image in attachments::images(attachment) {
                let Some(block) = image_block(&blobs.join(interpret::to_hex(&image.hash))) else {
                    continue;
                };
                flush(&mut prose, &mut blocks);
                blocks.push(block);
                sent_as_image = matches!(attachment.of, Some(Of::Image(_)));
            }
        }
        // A file, a review's patch, or an image the agent cannot see as
        // one (an SVG, say), embedded where the agent takes it.
        let file = match &attachment.of {
            Some(Of::File(blob)) => Some(blob),
            Some(Of::Image(blob)) if !sent_as_image => Some(blob),
            Some(Of::Review(review)) => review.diff.as_ref().and_then(|diff| diff.patch.as_ref()),
            _ => None,
        };
        if takes.embedded
            && let Some(blob) = file
            && let Some(block) =
                resource_block(&blobs.join(interpret::to_hex(&blob.hash)), &blob.mime)
        {
            flush(&mut prose, &mut blocks);
            blocks.push(block);
        }
    }
    flush(&mut prose, &mut blocks);
    blocks
}

/// An image block for the file at `path`, for bytes that are an image
/// every model reads (the agents' own image recordings sent PNG); None for
/// anything else, which goes as its element.
fn image_block(path: &Path) -> Option<ContentBlock> {
    let bytes = std::fs::read(path).ok()?;
    let mime = attachments::native_image(&bytes)?;
    Some(ContentBlock::Image(ImageContent {
        annotations: None,
        data: base64::engine::general_purpose::STANDARD.encode(&bytes),
        mime_type: mime.to_owned(),
        uri: None,
        meta: None,
        extra: Extra::new(),
    }))
}

/// A file's contents as an embedded resource, named by its path: text
/// where it is UTF-8, else its bytes.
fn resource_block(path: &Path, mime: &str) -> Option<ContentBlock> {
    let bytes = std::fs::read(path).ok()?;
    let uri = file_uri(path);
    let mime_type = (!mime.is_empty()).then(|| Some(mime.to_owned()));
    let resource = match String::from_utf8(bytes) {
        Ok(text) => EmbeddedResourceResource::Text(TextResourceContents {
            mime_type,
            text,
            uri,
            meta: None,
            extra: Extra::new(),
        }),
        Err(bytes) => EmbeddedResourceResource::Blob(BlobResourceContents {
            blob: base64::engine::general_purpose::STANDARD.encode(bytes.as_bytes()),
            mime_type,
            uri,
            meta: None,
            extra: Extra::new(),
        }),
    };
    Some(ContentBlock::Resource(EmbeddedResource {
        annotations: None,
        resource,
        meta: None,
        extra: Extra::new(),
    }))
}

/// A `file:` URI for an absolute path, with Windows' drive and separators
/// written as URIs write them.
fn file_uri(path: &Path) -> String {
    let path = path.to_string_lossy().replace('\\', "/");
    if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{path}")
    }
}

/// The program to start for `command`. On Windows a bare name is looked up
/// on the PATH under each of PATHEXT's extensions, as a shell would: an
/// agent installed by npm is a `.cmd` shim, which starting the name alone
/// never finds. Anywhere else, and for a path, the command as given.
pub fn program(command: &str, env: &std::collections::HashMap<String, String>) -> PathBuf {
    #[cfg(windows)]
    {
        let variable = |name: &str| {
            env.iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| std::ffi::OsString::from(value))
                .or_else(|| std::env::var_os(name))
        };
        let path = variable("PATH").unwrap_or_default();
        let extensions = variable("PATHEXT")
            .and_then(|value| value.into_string().ok())
            .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_owned());
        if let Some(found) = find_program(command, &path, &extensions, |path| path.is_file()) {
            return found;
        }
    }
    #[cfg(not(windows))]
    let _ = env;
    PathBuf::from(command)
}

/// Where a bare program name is found on `path` under one of `extensions`
/// (a `;`-separated list, PATHEXT's form): None for a name holding a
/// folder or an extension already, and for one found nowhere.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn find_program(
    name: &str,
    path: &std::ffi::OsStr,
    extensions: &str,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if name.contains(['/', '\\']) || Path::new(name).extension().is_some() {
        return None;
    }
    std::env::split_paths(path).find_map(|folder| {
        extensions
            .split(';')
            .filter(|extension| !extension.is_empty())
            .map(|extension| folder.join(format!("{name}{}", extension.to_ascii_lowercase())))
            .find(|candidate| exists(candidate))
    })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::Takes;

    fn blob(hash: u8, name: &str, mime: &str) -> wire::BlobRef {
        wire::BlobRef {
            hash: vec![hash; 32],
            name: name.into(),
            mime: mime.into(),
            size: 3,
        }
    }

    fn prompt(text: &str) -> Vec<u8> {
        acp_protocol::encode(&acp_protocol::ClientMessage::request(
            acp_protocol::RequestId::String("amux-prompt-01".into()),
            acp_protocol::ClientRequest::SessionPrompt(acp_protocol::client::PromptRequest {
                session_id: "s1".into(),
                prompt: vec![acp_protocol::ContentBlock::text(text)],
                meta: None,
                extra: Default::default(),
            }),
        ))
    }

    fn written(request: &[u8]) -> Vec<serde_json::Value> {
        let line: serde_json::Value = serde_json::from_slice(request).unwrap();
        line["params"]["prompt"].as_array().unwrap().clone()
    }

    /// An image goes as its element and then its bytes, the way the
    /// agents' own image recordings sent one (`agy-image`); a file goes as
    /// its element and then its contents, embedded and named by its path.
    #[test]
    fn an_image_and_a_file_go_as_blocks_after_their_elements() {
        let blobs = tempfile::tempdir().unwrap();
        std::fs::write(blobs.path().join("ab".repeat(32)), b"\x89PNG\r\n\x1a\nrest").unwrap();
        std::fs::write(blobs.path().join("cd".repeat(32)), "kiwi\n").unwrap();
        let attachments = [
            wire::Attachment {
                of: Some(wire::attachment::Of::Image(blob(
                    0xab,
                    "red.png",
                    "image/png",
                ))),
            },
            wire::Attachment {
                of: Some(wire::attachment::Of::File(blob(
                    0xcd,
                    "notes.txt",
                    "text/plain",
                ))),
            },
        ];
        let text = format!(
            "look {} and {} please",
            attachments::PLACEHOLDER,
            attachments::PLACEHOLDER
        );
        let takes = Takes {
            images: true,
            embedded: true,
        };
        let request =
            super::prompt_with_attachments(&prompt(&text), &attachments, blobs.path(), takes)
                .unwrap();
        let blocks = written(&request);
        let kinds: Vec<&str> = blocks
            .iter()
            .map(|block| block["type"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            ["text", "image", "text", "resource", "text"],
            "{blocks:?}"
        );
        assert!(
            blocks[0]["text"]
                .as_str()
                .unwrap()
                .starts_with("look <amux-attachment kind=\"image\"")
        );
        assert_eq!(blocks[1]["mimeType"], "image/png");
        assert_eq!(blocks[3]["resource"]["text"], "kiwi\n");
        assert_eq!(blocks[3]["resource"]["mimeType"], "text/plain");
        let path = blobs.path().join("cd".repeat(32));
        assert!(
            blocks[3]["resource"]["uri"]
                .as_str()
                .unwrap()
                .ends_with(&path.to_string_lossy().replace('\\', "/")),
            "{blocks:?}"
        );
        assert_eq!(blocks[4]["text"], " please");
        // What goes is a request the protocol crate reads back whole.
        acp_protocol::strict_client(&request).unwrap();
    }

    /// An agent that takes neither images nor embedded files gets the
    /// elements alone, which name where each file is for it to open.
    #[test]
    fn an_agent_that_takes_neither_reads_the_elements() {
        let blobs = tempfile::tempdir().unwrap();
        std::fs::write(blobs.path().join("ab".repeat(32)), b"\x89PNG\r\n\x1a\nrest").unwrap();
        std::fs::write(blobs.path().join("cd".repeat(32)), "kiwi\n").unwrap();
        let attachments = [
            wire::Attachment {
                of: Some(wire::attachment::Of::Image(blob(
                    0xab,
                    "red.png",
                    "image/png",
                ))),
            },
            wire::Attachment {
                of: Some(wire::attachment::Of::File(blob(
                    0xcd,
                    "notes.txt",
                    "text/plain",
                ))),
            },
        ];
        let text = format!("{}{}", attachments::PLACEHOLDER, attachments::PLACEHOLDER);
        let request = super::prompt_with_attachments(
            &prompt(&text),
            &attachments,
            blobs.path(),
            Takes::default(),
        )
        .unwrap();
        let blocks = written(&request);
        assert_eq!(blocks.len(), 1, "{blocks:?}");
        let text = blocks[0]["text"].as_str().unwrap();
        assert!(
            text.contains(&blobs.path().join("cd".repeat(32)).display().to_string()),
            "{text}"
        );
    }

    /// An SVG is no image a model reads: it goes as a file, embedded.
    #[test]
    fn an_image_no_model_reads_goes_as_a_file() {
        let blobs = tempfile::tempdir().unwrap();
        std::fs::write(blobs.path().join("ab".repeat(32)), "<svg/>").unwrap();
        let attachments = [wire::Attachment {
            of: Some(wire::attachment::Of::Image(blob(
                0xab,
                "logo.svg",
                "image/svg+xml",
            ))),
        }];
        let request = super::prompt_with_attachments(
            &prompt(&attachments::PLACEHOLDER.to_string()),
            &attachments,
            blobs.path(),
            Takes {
                images: true,
                embedded: true,
            },
        )
        .unwrap();
        let kinds: Vec<String> = written(&request)
            .iter()
            .map(|block| block["type"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(kinds, ["text", "resource"]);
    }

    /// The session's requests carry amux's tool server as a stdio entry,
    /// and the extra folders only where the agent takes them.
    #[test]
    fn a_session_is_opened_with_amuxs_tool_server() {
        let setup = super::SessionSetup {
            cwd: "/work".into(),
            folders: vec!["/agents/a/blobs".into()],
            tool_server: Some((
                "/opt/amux".into(),
                vec!["internal".into(), "mcp".into(), "/agents/a".into()],
            )),
        };
        let line = |opening, folders| -> serde_json::Value {
            serde_json::from_slice(&super::open_session(&opening, &setup, folders).unwrap())
                .unwrap()
        };
        let new = line(interpret::acp::Opening::New, false);
        assert_eq!(new["method"], "session/new");
        assert_eq!(new["id"], interpret::acp::NEW_SESSION_REQUEST);
        assert_eq!(
            new["params"]["mcpServers"],
            serde_json::json!([{
                "name": "amux",
                "command": "/opt/amux",
                "args": ["internal", "mcp", "/agents/a"],
                "env": [],
            }])
        );
        assert!(new["params"].get("additionalDirectories").is_none());
        let resume = line(interpret::acp::Opening::Resume("s1".into()), true);
        assert_eq!(resume["method"], "session/resume");
        assert_eq!(resume["params"]["sessionId"], "s1");
        assert_eq!(
            resume["params"]["additionalDirectories"],
            serde_json::json!(["/agents/a/blobs"])
        );
        let load = line(interpret::acp::Opening::Load("s1".into()), false);
        assert_eq!(load["method"], "session/load");
        assert_eq!(load["params"]["mcpServers"][0]["name"], "amux");
        assert!(super::open_session(&interpret::acp::Opening::Unresumable, &setup, true).is_none());
    }

    /// A bare name is found under PATHEXT's extensions in PATH's order, as
    /// npm's `.cmd` shims need on Windows; a path or a name with an
    /// extension is left as given.
    #[test]
    fn a_bare_name_is_found_under_each_extension() {
        let folders = [PathBuf::from("/a"), PathBuf::from("/b")];
        let path = std::env::join_paths(&folders).unwrap();
        let exists = |path: &Path| path == Path::new("/b/opencode.cmd");
        assert_eq!(
            super::find_program("opencode", &path, ".COM;.EXE;.BAT;.CMD", exists),
            Some(PathBuf::from("/b/opencode.cmd"))
        );
        assert_eq!(
            super::find_program("opencode.cmd", &path, ".CMD", exists),
            None
        );
        assert_eq!(
            super::find_program("/b/opencode", &path, ".CMD", exists),
            None
        );
        assert_eq!(super::find_program("grok", &path, ".CMD", exists), None);
    }
}
