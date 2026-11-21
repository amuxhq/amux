use prost::Message as _;

use super::*;

fn call(request: Value) -> Value {
    let answer: Value = serde_json::from_str(&handle(&request.to_string())).unwrap();
    match answer.get("ok") {
        Some(ok) => ok.clone(),
        None => panic!("{request} failed: {answer}"),
    }
}

fn b64(message: &impl prost::Message) -> String {
    STANDARD.encode(message.encode_to_vec())
}

fn inventory(event: wire::inventory_event::Of) -> String {
    b64(&wire::SubscribeInventoryResponse {
        event: Some(wire::InventoryEvent { of: Some(event) }),
    })
}

const HOST: [u8; 16] = [7; 16];
const AGENT: [u8; 16] = [9; 16];

fn fleet_with_one_agent(kind: wire::Kind) -> u32 {
    let fleet = call(json!({"op": "fleetNew"})).as_u64().unwrap() as u32;
    let host = wire::HostEntry {
        host: Some(wire::Host {
            host_id: HOST.to_vec(),
            name: "desk".into(),
            ..Default::default()
        }),
        providers: vec![wire::ProviderOnHost {
            provider_id: "claude".into(),
            descriptor: Some(wire::ProviderDescriptor {
                name: "Claude".into(),
                kind: wire::Kind::ClaudeSdk as i32,
                ..Default::default()
            }),
            ..Default::default()
        }],
        standing: Some(wire::host_entry::Standing::Trusted(Default::default())),
        ..Default::default()
    };
    let agent = wire::Agent {
        agent_id: AGENT.to_vec(),
        host_id: HOST.to_vec(),
        kind: kind as i32,
        name: "site".into(),
        cwd: "/work/site".into(),
        ..Default::default()
    };
    for event in [
        wire::inventory_event::Of::Host(host),
        wire::inventory_event::Of::Agent(agent),
        wire::inventory_event::Of::CaughtUp(Default::default()),
    ] {
        call(json!({"op": "fleetApply", "fleet": fleet, "message": inventory(event)}));
    }
    fleet
}

#[test]
fn the_picker_draws_the_fleet_as_home_does() {
    let fleet = fleet_with_one_agent(wire::Kind::Codex);
    let view = call(json!({"op": "fleetView", "fleet": fleet, "localHost": hex(&HOST)}));
    let card = &view["sections"][0]["rows"][0]["card"];
    assert_eq!(card["handle"], "site");
    assert_eq!(card["cwd"], "/work/site");
    let picker =
        call(json!({"op": "picker", "fleet": fleet, "localHost": hex(&HOST), "profile": "ab"}));
    assert_eq!(picker[0]["handle"], "site");
    assert_eq!(picker[0]["agent"], hex(&AGENT));
    assert_eq!(picker[0]["local"], true);
    assert_eq!(picker[0]["closed"], false);
    assert_eq!(
        call(json!({"op": "agentKind", "fleet": fleet, "host": hex(&HOST), "agent": hex(&AGENT)})),
        "KIND_CODEX"
    );
    assert_eq!(
        call(json!({"op": "agentKind", "fleet": fleet, "host": hex(&HOST), "agent": "00"})),
        Value::Null
    );
    call(json!({"op": "fleetDrop", "fleet": fleet}));
    let gone: Value = serde_json::from_str(&handle(
        &json!({"op": "fleetView", "fleet": fleet, "localHost": ""}).to_string(),
    ))
    .unwrap();
    assert!(gone["error"].as_str().unwrap().contains("no fleet"));
}

/// A review goes in the prompt arm of the agent's own kind.
#[test]
fn a_page_review_is_sent_in_the_agents_kind() {
    let fleet = fleet_with_one_agent(wire::Kind::Codex);
    let page = wire::PageReview {
        url: "http://localhost:5173/".into(),
        title: "Home".into(),
        comments: vec![wire::PageComment {
            text: "Bigger".into(),
            element: Some(wire::PageElement {
                selector: "h1".into(),
                text: "Welcome".into(),
                source: String::new(),
            }),
            quote: String::new(),
            image: None,
        }],
    };
    let body = call(json!({
        "op": "sendPrompt",
        "fleet": fleet,
        "host": hex(&HOST),
        "agent": hex(&AGENT),
        "inputId": "00112233445566778899aabbccddeeff",
        "text": "Fix \u{FFFC}",
        "pages": [serde_json::to_value(&page).unwrap()],
    }));
    let request =
        wire::SendInputRequest::decode(STANDARD.decode(body.as_str().unwrap()).unwrap().as_slice())
            .unwrap();
    assert_eq!(request.agent_id, AGENT);
    let input = request.input.unwrap();
    assert_eq!(input.kind(), wire::Kind::Codex);
    let Some(wire::KindInput::Codex(wire::CodexInput {
        of: Some(wire::codex_input::Of::Prompt(prompt)),
    })) = input.decode_body()
    else {
        panic!("not a Codex prompt");
    };
    assert_eq!(prompt.text, "Fix \u{FFFC}");
    assert_eq!(
        prompt.attachments,
        [wire::Attachment {
            of: Some(wire::attachment::Of::Page(page))
        }]
    );
}

#[test]
fn a_new_agent_is_the_kind_its_host_publishes() {
    let fleet = fleet_with_one_agent(wire::Kind::Codex);
    let body = call(json!({
        "op": "createAgent",
        "fleet": fleet,
        "localHost": hex(&HOST),
        "agent": hex(&AGENT),
        "cwd": "~/site",
    }));
    let request = wire::CreateAgentRequest::decode(
        STANDARD.decode(body.as_str().unwrap()).unwrap().as_slice(),
    )
    .unwrap();
    assert!(matches!(
        request.agent,
        Some(wire::create_agent_request::Agent::ClaudeSdk(_))
    ));
    assert_eq!(request.cwd, "~/site");
    assert!(request.initial_prompt.is_none());
}

#[test]
fn answers_read_back() {
    let profiles = b64(&wire::ListProfilesResponse {
        profiles: vec![wire::Profile {
            id: vec![0xab, 0x01],
            label: "work".into(),
            ..Default::default()
        }],
    });
    assert_eq!(
        call(json!({"op": "profiles", "message": profiles})),
        json!([{"id": "ab01", "label": "work", "email": "", "accountName": ""}])
    );
    let verdict = b64(&wire::SendInputResponse {
        of: Some(wire::send_input_response::Of::Rejected(wire::Rejected {
            refusal: wire::Refusal::NotQueued as i32,
            detail: Some("stopped".into()),
        })),
    });
    let verdict = call(json!({"op": "verdict", "message": verdict}));
    assert_eq!(verdict["accepted"], false);
    assert_eq!(verdict["detail"], "stopped");
    let blob = b64(&wire::PutBlobResponse {
        blob: Some(wire::BlobRef {
            hash: vec![1; 32],
            name: "region-1.png".into(),
            mime: "image/png".into(),
            size: 5,
        }),
    });
    let blob = call(json!({"op": "blob", "message": blob}));
    assert_eq!(blob["name"], "region-1.png");
    let bad: Value = serde_json::from_str(&handle("{\"op\": \"nonsense\"}")).unwrap();
    assert!(bad["error"].is_string());
}
