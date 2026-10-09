use std::path::Path;

mod door;
mod golden;
mod release;
use xtask::ios_verify;
mod simulator;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match std::env::args().nth(1).as_deref() {
        Some("codegen") => codegen(),
        Some("door") => door::main(),
        Some("golden") => golden::main(),
        Some("ios-verify") => ios_verify::run(),
        Some("swift-types") => {
            xtask::swift_types::main(&std::env::args().skip(2).collect::<Vec<_>>())
        }
        Some("release") => release::main(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("restamp") => restamp(&std::env::args().skip(2).collect::<Vec<_>>()),
        _ => {
            eprintln!(
                "usage: xtask <codegen|swift-types [--check]|golden diff [ARGS]|restamp FROM TO [VERSION]|release <key generate|key public|cut VERSION|deploy VERSION [--channel C] [--rollout N]>|door [--simulator NAME] [--bundle-id ID] [--install APP] [--timeout SECS] [--requests FILE] [JSON...]|ios-verify>"
            );
            std::process::exit(2);
        }
    }
}

/// Copies the amux binary at FROM to TO stamped with VERSION, by default
/// one just below its own, to play the previous release in overlap tests.
fn restamp(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let (Some(from), Some(to)) = (args.first(), args.get(1)) else {
        return Err("usage: xtask restamp FROM TO [VERSION]".into());
    };
    let (from, to) = (Path::new(from), Path::new(to));
    let version = match args.get(2) {
        Some(version) => version.clone(),
        None => {
            let own = version_stamp::read_file(from)?;
            version_stamp::previous(&own)
                .ok_or_else(|| format!("{own} has a prerelease; name the version to stamp"))?
        }
    };
    version_stamp::restamp(from, to, &version)?;
    println!("{} reports {version}", to.display());
    Ok(())
}

/// Wire types a view value holds.
const VIEW_VALUES: &[&str] = &[
    "amux.v1.BlobRef",
    "amux.v1.BoundaryKind",
    "amux.v1.Catalogue",
    "amux.v1.Diff",
    "amux.v1.DiffBase",
    "amux.v1.DiffBase.base",
    "amux.v1.DiffFile",
    "amux.v1.Empty",
    "amux.v1.EnvelopeKind",
    "amux.v1.RouteKind",
    "amux.v1.Kind",
    "amux.v1.OfferedCommand",
    "amux.v1.OfferedMode",
    "amux.v1.OfferedModel",
    "amux.v1.OfferedPermission",
    "amux.v1.Provider",
    "amux.v1.ReviewComment",
    "amux.v1.ReviewStatus",
    "amux.v1.SendState",
    "amux.v1.SignInState",
    "amux.v1.UsageState",
];

/// The proto files that make up the wire, relative to the proto root.
const PROTO_FILES: &[&str] = &[
    "amux/v1/error.proto",
    "amux/v1/link.proto",
    "amux/v1/pairing.proto",
    "amux/v1/front_door.proto",
    "amux/v1/service.proto",
    "amux/v1/records.proto",
    "amux/v1/chat.proto",
    "amux/v1/agent.proto",
    "amux/v1/claude.proto",
    "amux/v1/codex.proto",
];

/// Regenerates the committed protobuf code under
/// `crates/wire/src/generated/` from `crates/wire/proto/`.
///
/// The output is committed rather than built in `OUT_DIR` so that the
/// generated Rust is an ordinary tracked input — visible to git, review,
/// rust-analyzer, and every build cache — and so building amux needs no
/// protoc. CI regenerates and fails if the committed output is stale.
fn codegen() -> Result<(), Box<dyn std::error::Error>> {
    let wire_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives under crates/")
        .join("wire");
    let proto_dir = wire_dir.join("proto");
    let out_dir = wire_dir.join("src/generated");
    std::fs::create_dir_all(&out_dir)?;

    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    // SAFETY: set before any codegen work and never mutated again; xtask is a
    // short-lived single-threaded process. The vendored protoc keeps the
    // output independent of whatever protoc the host has installed.
    unsafe {
        std::env::set_var("PROTOC", protoc);
    }

    // Type names let code that walks an encoded message by its descriptor,
    // such as the dump redactor, find the descriptor of a generated type.
    let mut config = tonic_prost_build::Config::new();
    config.enable_type_names();
    // The wire values the chat views carry to the phone serialize, with
    // schemas, so the Swift mirrors are generated from these definitions.
    for path in VIEW_VALUES {
        config.type_attribute(
            path,
            "#[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)]",
        );
    }
    // prost never derives Eq for a message with a repeated message field,
    // though every field here is Eq; the views that hold a Diff compare it.
    config.type_attribute("amux.v1.Diff", "#[derive(Eq)]");
    tonic_prost_build::configure()
        // Keep generated clients, but omit tonic's transport convenience
        // constructors. Otherwise `RoutingService.Connect` collides with the
        // inherent `RoutingServiceClient::connect(endpoint)` constructor.
        .build_transport(false)
        .file_descriptor_set_path(out_dir.join("amux.v1.bin"))
        .out_dir(&out_dir)
        .compile_with_config(
            config,
            &PROTO_FILES
                .iter()
                .map(|file| proto_dir.join(file))
                .collect::<Vec<_>>(),
            &[proto_dir],
        )?;
    Ok(())
}
