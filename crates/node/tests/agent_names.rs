//! Every agent has a name: the one it was given, which must be lowercase
//! letters, digits and hyphens, else a word pair the daemon chose that no other agent on the host has and no branch in the
//! folder's repository has, so a branch can later be named after it.

use std::path::Path;
use std::process::Command;

use prost::Message as _;
use support::*;
use wire::AgentSpec;

use crate::support;

fn spec_name(install: &Install, agent: &wire::Agent) -> String {
    let bytes = std::fs::read(install.agent_dir(id_of(agent)).join("spec.1")).unwrap();
    AgentSpec::decode(bytes.as_slice()).unwrap().name
}

fn unnamed(cwd: &Path) -> wire::CreateAgentRequest {
    wire::CreateAgentRequest {
        name: None,
        ..create(cwd, "", None)
    }
}

fn git(cwd: &Path, args: &[&str], stdin: Option<&str>) -> String {
    use std::io::Write as _;
    let mut child = Command::new("git")
        .args([
            "-c",
            "user.name=amux",
            "-c",
            "user.email=amux@example.invalid",
        ])
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.unwrap_or_default().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "git {args:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_given_name_is_kept_and_a_missing_one_is_a_word_pair() {
    let install = Install::new();
    let daemon = install
        .start("boot-1", install.launch("idle", vec![]))
        .await;
    let runtime = runtime(&daemon, &install);

    let given = runtime
        .spawn(create(&install.work, "release-notes", None), None)
        .await
        .unwrap();
    assert_eq!(given.name, "release-notes");
    assert_eq!(spec_name(&install, &given), "release-notes");

    let assigned = runtime.spawn(unnamed(&install.work), None).await.unwrap();
    assert!(
        node::word_pairs().any(|pair| pair == assigned.name),
        "{:?} is a word pair",
        assigned.name
    );
    assert_eq!(spec_name(&install, &assigned), assigned.name);
    assert_eq!(
        runtime.agent(id_of(&assigned)).await.unwrap().name,
        assigned.name
    );
    println!("given: {}, assigned: {}", given.name, assigned.name);

    let emptied = runtime.rename(id_of(&assigned), "").await;
    assert!(
        matches!(emptied, Err(node::RegistryError::BadName(_))),
        "a name cannot be taken away: {emptied:?}"
    );

    // A name is one word a branch, a folder and a command line all take as
    // it is: "fix login" is refused, by a create and by a rename alike, and
    // the refused create makes no agent.
    for bad in ["fix login", "Fix-login", "-fix", "fix/login", "fix.login"] {
        let request = create(&install.work, bad, None);
        let id = uuid::Uuid::from_slice(&request.agent_id).unwrap();
        let created = runtime.spawn(request, None).await;
        assert!(
            matches!(created, Err(node::RegistryError::BadName(_))),
            "{bad:?} cannot name a new agent: {created:?}"
        );
        assert!(runtime.agent(id).await.is_err(), "{bad:?} made no agent");
        let renamed = runtime.rename(id_of(&assigned), bad).await;
        assert!(
            matches!(renamed, Err(node::RegistryError::BadName(_))),
            "{bad:?} cannot rename an agent: {renamed:?}"
        );
    }
    assert_eq!(
        runtime.agent(id_of(&assigned)).await.unwrap().name,
        assigned.name
    );

    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_assigned_name_avoids_other_agents_and_the_repositorys_branches() {
    let install = Install::new();
    let repo = install.path("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"], None);
    git(
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "start"],
        None,
    );
    let head = git(&repo, &["rev-parse", "HEAD"], None);

    // Three pairs stay free of branches: the first is another agent's
    // name, the second a folder a branch sits under; only the third is
    // left to take.
    let pairs: Vec<String> = node::word_pairs().collect();
    let (agent_named, folder, free) = (&pairs[100], &pairs[1000], &pairs[2000]);
    let mut refs: String = pairs
        .iter()
        .filter(|pair| ![agent_named, folder, free].contains(pair))
        .map(|pair| format!("create refs/heads/{pair} {head}\n"))
        .collect();
    refs.push_str(&format!("create refs/heads/{folder}/feature {head}\n"));
    git(&repo, &["update-ref", "--stdin"], Some(&refs));

    let daemon = install
        .start("boot-1", install.launch("idle", vec![]))
        .await;
    let runtime = runtime(&daemon, &install);
    runtime
        .spawn(create(&install.work, agent_named, None), None)
        .await
        .unwrap();

    let assigned = runtime.spawn(unnamed(&repo), None).await.unwrap();
    assert_eq!(
        &assigned.name, free,
        "every other pair is a branch, a branch's folder or another agent's name"
    );

    let numbered = runtime.spawn(unnamed(&repo), None).await.unwrap();
    let (pair, number) = numbered.name.rsplit_once('-').unwrap();
    assert!(
        pairs.iter().any(|known| known == pair) && number == "2",
        "with every pair taken a number follows one: {:?}",
        numbered.name
    );
    println!(
        "{} branches; other agent {agent_named}; assigned {} then {}",
        pairs.len() - 2,
        assigned.name,
        numbered.name
    );

    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// Names are unique on a host, counting exited agents, which stay
/// resumable and addressable by name: a spawn asking for a taken name gets
/// the next free suffix (worker-2, worker-3) and is told the name it got,
/// as a spawn already returns its agent. QA: a lead spawned two children
/// called worker and could then message and stop neither.
#[tokio::test(flavor = "multi_thread")]
async fn a_spawn_with_a_taken_name_gets_the_next_free_suffix_counting_exited_agents() {
    let install = Install::new();
    let daemon = install
        .start("boot-1", install.launch("idle", vec![]))
        .await;
    let runtime = runtime(&daemon, &install);
    let first = runtime
        .spawn(create(&install.work, "worker", None), None)
        .await
        .unwrap();
    kill_all(&runtime).await;
    assert!(
        runtime.live().is_empty(),
        "the first worker exited and stays listed"
    );
    let second = runtime
        .spawn(create(&install.work, "worker", None), None)
        .await
        .unwrap();
    let third = runtime
        .spawn(create(&install.work, "worker", None), None)
        .await
        .unwrap();
    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
    let names = [&first.name, &second.name, &third.name];
    println!("spawned as {names:?}");
    assert_eq!(
        names,
        [
            &"worker".to_owned(),
            &"worker-2".to_owned(),
            &"worker-3".to_owned()
        ],
        "a taken name, an exited agent's included, gets the next free suffix"
    );
}

/// A rename to a name another agent on the host holds is refused with the
/// reason; a person renaming an agent must not make two agents answer to
/// one name.
#[tokio::test(flavor = "multi_thread")]
async fn a_rename_to_a_taken_name_is_refused() {
    let install = Install::new();
    let daemon = install
        .start("boot-1", install.launch("idle", vec![]))
        .await;
    let runtime = runtime(&daemon, &install);
    runtime
        .spawn(create(&install.work, "alpha", None), None)
        .await
        .unwrap();
    let beta = runtime
        .spawn(create(&install.work, "beta", None), None)
        .await
        .unwrap();
    let renamed = runtime.rename(id_of(&beta), "alpha").await;
    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
    println!("renaming beta to alpha: {renamed:?}");
    assert!(
        renamed.is_err(),
        "beta was renamed to alpha, a name another agent holds: {renamed:?}"
    );
}

/// Every id form amux prints picks its agent, at the one resolution an
/// agent's send and stop tools and the CLI share: the full id with
/// dashes, without them (as the ambiguity refusal printed it), and the
/// eight-character short id `amux ls` prints. QA: ids the refusal listed
/// were then refused as "no agent named <id>".
#[tokio::test(flavor = "multi_thread")]
async fn every_id_form_amux_prints_resolves_to_its_agent() {
    let install = Install::new();
    let daemon = install
        .start("boot-1", install.launch("idle", vec![]))
        .await;
    let runtime = runtime(&daemon, &install);
    let agent = runtime
        .spawn(create(&install.work, "worker", None), None)
        .await
        .unwrap();
    let id = id_of(&agent);
    let simple = id.simple().to_string();
    let forms = [id.to_string(), simple.clone(), simple[..8].to_owned()];
    let mut refused = Vec::new();
    for form in &forms {
        match runtime.resolve_agent(form).await {
            Ok(found) if found.agent_id == agent.agent_id => {}
            other => refused.push((form.clone(), format!("{other:?}"))),
        }
    }
    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
    assert!(
        refused.is_empty(),
        "id forms amux prints do not pick their agent: {refused:?}"
    );
}
