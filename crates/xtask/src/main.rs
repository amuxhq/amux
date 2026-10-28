use std::path::Path;

mod release;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match std::env::args().nth(1).as_deref() {
        Some("codegen") => codegen(),
        Some("proto-compat") => {
            xtask::proto_compat::main(&std::env::args().skip(2).collect::<Vec<_>>())
        }
        Some("release") => release::main(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("restamp") => restamp(&std::env::args().skip(2).collect::<Vec<_>>()),
        _ => {
            eprintln!(
                "usage: xtask <codegen|proto-compat [REF]|restamp FROM TO [VERSION]|release <key generate|key public|cut VERSION|deploy VERSION [--channel C] [--rollout N]>>"
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
    "amux.v1.BackgroundKind",
    "amux.v1.BlobRef",
    "amux.v1.BoundaryKind",
    "amux.v1.Catalogue",
    "amux.v1.ChangeTotals",
    "amux.v1.Diff",
    "amux.v1.DiffBase",
    "amux.v1.DiffBase.base",
    "amux.v1.DiffFile",
    "amux.v1.Empty",
    "amux.v1.EnvelopeKind",
    "amux.v1.Git",
    "amux.v1.Kind",
    "amux.v1.OfferedCommand",
    "amux.v1.OfferedSkill",
    "amux.v1.OfferedMode",
    "amux.v1.OfferedModel",
    "amux.v1.OfferedPermission",
    "amux.v1.Platform",
    "amux.v1.Provider",
    "amux.v1.ReviewComment",
    "amux.v1.ReviewStatus",
    "amux.v1.RouteKind",
    "amux.v1.SendState",
    "amux.v1.SignInState",
    "amux.v1.UsageState",
];

/// The proto files of the public contract, relative to `proto/` at the
/// repository's root.
const PROTO_FILES: &[&str] = &[
    "amux/v1/error.proto",
    "amux/v1/link.proto",
    "amux/v1/records.proto",
    "amux/v1/chat.proto",
    "amux/v1/inventory.proto",
    "amux/v1/dump.proto",
    "amux/v1/agent_service.proto",
    "amux/v1/session_service.proto",
    "amux/v1/peer_service.proto",
    "amux/v1/installation_service.proto",
    "amux/v1/profile_admin_service.proto",
    "amux/v1/pairing_service.proto",
    "amux/v1/agent_process.proto",
    "amux/v1/claude.proto",
    "amux/v1/codex.proto",
    "google/rpc/code.proto",
    "google/rpc/status.proto",
    "google/rpc/error_details.proto",
];

/// The proto files only amux reads, relative to `crates/wire/proto/`: what
/// a client writes into a dump, outside the contract's promises.
const INTERNAL_PROTO_FILES: &[&str] = &["amux/dump/session.proto"];

/// Regenerates the committed protobuf code under
/// `crates/wire/src/generated/` from `proto/` and `crates/wire/proto/`.
///
/// The output is committed rather than built in `OUT_DIR` so that the
/// generated Rust is an ordinary tracked input — visible to git, review,
/// rust-analyzer, and every build cache — and so building amux needs no
/// protoc. CI regenerates and fails if the committed output is stale.
fn codegen() -> Result<(), Box<dyn std::error::Error>> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives under crates/");
    let wire_dir = crates.join("wire");
    let contract_dir = crates
        .parent()
        .expect("crates/ lives in the repository")
        .join("proto");
    let internal_dir = wire_dir.join("proto");
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
    // A detail packed as Any names its type by URL, gRPC's richer error
    // model's way.
    config.type_name_domain(["."], "type.googleapis.com");
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
        // The services need tonic and through it tokio; the messages need
        // neither, so the cores that only speak the messages leave the
        // services out.
        .client_mod_attribute(".", "#[cfg(feature = \"grpc\")]")
        .server_mod_attribute(".", "#[cfg(feature = \"grpc\")]")
        .file_descriptor_set_path(out_dir.join("amux.v1.bin"))
        .out_dir(&out_dir)
        .compile_with_config(
            config,
            &PROTO_FILES
                .iter()
                .map(|file| contract_dir.join(file))
                .chain(
                    INTERNAL_PROTO_FILES
                        .iter()
                        .map(|file| internal_dir.join(file)),
                )
                .collect::<Vec<_>>(),
            // The vendored protoc's own include directory holds the
            // well-known types the googleapis files import.
            &[
                contract_dir,
                internal_dir,
                protoc_bin_vendored::include_path()?,
            ],
        )?;
    Ok(())
}
