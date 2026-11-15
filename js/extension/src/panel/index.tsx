// The side panel: the tab's pending comments, where they go, and Send, the
// shape of the diff review page. It follows the active tab of its window.

import { render } from "preact";
import { useEffect, useMemo, useRef, useState } from "preact/hooks";
import type {
  AgentChoice,
  Destination,
  FromPanel,
  PanelState,
  Reach,
  ToPanel,
} from "../shared/messages.ts";

const port = chrome.runtime.connect({ name: "panel" });
const post = (message: FromPanel) => port.postMessage(message);
const isMac = navigator.platform.toLowerCase().includes("mac");
const shortcut = isMac ? "⌘⇧." : "Ctrl+Shift+.";

/**
 * The tab a panel opened as an ordinary page shows, named by `?tab=`: in its
 * own tab the active tab is the panel itself. The end-to-end test opens it so.
 */
const pinned = Number(new URLSearchParams(location.search).get("tab")) || null;

async function activeTab(): Promise<number | null> {
  if (pinned !== null) return pinned;
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  return tab?.id ?? null;
}

function same(a: Destination | null, b: Destination | null): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

function App() {
  const [state, setState] = useState<PanelState | null>(null);
  const [destination, setDestination] = useState<Destination | null>(null);
  const [message, setMessage] = useState("");
  const chosen = useRef(false);

  useEffect(() => {
    const listen = (incoming: ToPanel) => {
      if (incoming.type === "state") setState(incoming.state);
    };
    port.onMessage.addListener(listen);
    const follow = async () => {
      const tabId = await activeTab();
      if (tabId !== null) post({ type: "watch", tabId });
    };
    void follow();
    const activated = () => {
      chosen.current = false;
      setMessage("");
      void follow();
    };
    chrome.tabs.onActivated.addListener(activated);
    chrome.windows.onFocusChanged.addListener(activated);
    return () => {
      port.onMessage.removeListener(listen);
      chrome.tabs.onActivated.removeListener(activated);
      chrome.windows.onFocusChanged.removeListener(activated);
    };
  }, []);

  useEffect(() => {
    if (!state || chosen.current) return;
    if (!same(destination, state.suggested)) setDestination(state.suggested);
  }, [state?.suggested]);

  if (!state) return <main class="panel" />;
  const tabId = state.tabId;
  const { review } = state;
  const items = review.comments.length + review.snippets.length;

  return (
    <main class="panel">
      <header>
        <img src="icon.png" alt="" width={18} height={18} />
        <h1>Page review</h1>
        <Status reach={state.reach} />
      </header>
      <Problem state={state} />
      {review.comments.length === 0 && review.snippets.length === 0 ? (
        <Empty state={state} />
      ) : (
        <ol class="comments">
          {review.comments.map((comment, index) => (
            <li key={comment.id}>
              <button
                type="button"
                class="comment"
                title="Show it on the page"
                onClick={() => tabId !== null && post({ type: "focus", tabId, id: comment.id })}
              >
                <span class="number">{index + 1}</span>
                <span class="body">
                  <span class="target">
                    {comment.quote ? `“${comment.quote}”` : comment.region ? "Box" : comment.element.text || comment.element.selector}
                  </span>
                  {comment.text ? <span class="text">{comment.text}</span> : <span class="text none">No comment</span>}
                  {state.thumbnails[comment.id] && <img class="thumbnail" src={state.thumbnails[comment.id]} alt="" />}
                </span>
              </button>
              <button
                type="button"
                class="remove"
                aria-label="Delete comment"
                onClick={() => tabId !== null && post({ type: "delete", tabId, id: comment.id })}
              >
                ×
              </button>
            </li>
          ))}
          {review.snippets.map((snippet) => (
            <li key={snippet.id}>
              <div class="comment">
                <span class="number snippet">¶</span>
                <span class="body">
                  <span class="target">{snippet.name}</span>
                  <span class="text">{snippet.text.split("\n")[0]}</span>
                </span>
              </div>
              <button
                type="button"
                class="remove"
                aria-label="Remove"
                onClick={() => tabId !== null && post({ type: "deleteSnippet", tabId, id: snippet.id })}
              >
                ×
              </button>
            </li>
          ))}
        </ol>
      )}
      {items > 0 && !state.reviewing && state.blocked === null && review.comments.length > 0 && (
        <button type="button" class="link" onClick={() => tabId !== null && post({ type: "review", tabId })}>
          Add more on the page
        </button>
      )}
      <Composer
        state={state}
        destination={destination}
        onDestination={(next) => {
          chosen.current = true;
          setDestination(next);
        }}
        message={message}
        onMessage={setMessage}
        onSend={() => {
          if (tabId === null || !destination) return;
          post({ type: "send", tabId, destination, message });
          setMessage("");
        }}
      />
      {state.outcome && "sent" in state.outcome && <p class="outcome">Sent to {state.outcome.sent}.</p>}
      {state.outcome && "failed" in state.outcome && <p class="outcome failed">Not sent: {state.outcome.failed}</p>}
    </main>
  );
}

function Status({ reach }: { reach: Reach }) {
  const [words, kind] =
    reach.status === "ok"
      ? [`amux ${reach.version}`, "ok"]
      : reach.status === "connecting"
        ? ["Connecting…", "waiting"]
        : ["Not connected", "problem"];
  return (
    <span class={`status ${kind}`} title={words}>
      <span class="dot" />
      {words}
    </span>
  );
}

function Problem({ state }: { state: PanelState }) {
  const { reach, blocked } = state;
  if (reach.status === "not-installed") {
    return (
      <p class="notice">
        amux isn’t set up for this browser yet. Install or update amux from <a href="https://amux.sh" target="_blank">amux.sh</a>, then start it: it
        registers itself with your browsers when it starts.
      </p>
    );
  }
  if (reach.status === "not-running") {
    return (
      <p class="notice">
        amux isn’t running. Open the amux app, or run <code>amux server start</code>.
      </p>
    );
  }
  if (reach.status === "stopped") {
    return <p class="notice">amux’s connection stopped{reach.detail ? `: ${reach.detail}` : "."}</p>;
  }
  if (blocked === "browser-page") {
    return <p class="notice">The browser’s own pages can’t be reviewed. Open a website or a local file.</p>;
  }
  if (blocked === "file-access") {
    return (
      <div class="notice">
        <p>To review local files, turn on “Allow access to file URLs” for amux.</p>
        <button type="button" onClick={() => post({ type: "fileAccess" })}>
          Open amux’s settings
        </button>
      </div>
    );
  }
  return null;
}

function Empty({ state }: { state: PanelState }) {
  if (state.blocked !== null) return null;
  if (state.reviewing) {
    return (
      <div class="empty">
        <p>Click a block, select text, or drag a box on the page.</p>
        <p class="muted">Hold Shift to use the page normally. Esc when you’re done.</p>
      </div>
    );
  }
  return (
    <div class="empty">
      <button
        type="button"
        class="primary"
        onClick={() => state.tabId !== null && post({ type: "review", tabId: state.tabId })}
      >
        Review this page
      </button>
      <p class="muted">
        Or press {shortcut}. Then click a block, select text, or drag a box, and write what you think. Send them all to
        an agent together.
      </p>
    </div>
  );
}

interface ComposerProps {
  state: PanelState;
  destination: Destination | null;
  onDestination(destination: Destination | null): void;
  message: string;
  onMessage(message: string): void;
  onSend(): void;
}

function Composer({ state, destination, onDestination, message, onMessage, onSend }: ComposerProps) {
  const { review } = state;
  const count = review.comments.length + review.snippets.length;
  const groups = useMemo(() => {
    const byProfile = new Map<string, { label: string; agents: AgentChoice[] }>();
    for (const profile of state.profiles) byProfile.set(profile.id, { label: profile.label, agents: [] });
    for (const agent of state.agents) {
      if (agent.closed) continue;
      byProfile.get(agent.profile)?.agents.push(agent);
    }
    return [...byProfile.entries()];
  }, [state.agents, state.profiles]);
  if (count === 0 || state.reach.status !== "ok") return null;
  const many = state.profiles.length > 1;
  const value = destination ? JSON.stringify(destination) : "";
  const noun = review.comments.length > 0 ? (review.comments.length === 1 ? "comment" : "comments") : "";
  const label =
    review.comments.length > 0
      ? `Send ${review.comments.length} ${noun}`
      : review.snippets.length === 1
        ? "Send"
        : `Send ${review.snippets.length}`;
  return (
    <form
      class="composer"
      onSubmit={(event) => {
        event.preventDefault();
        onSend();
      }}
    >
      <label>
        <span>To</span>
        <select
          value={value}
          onChange={(event) => {
            const picked = (event.target as HTMLSelectElement).value;
            onDestination(picked ? (JSON.parse(picked) as Destination) : null);
          }}
        >
          <option value="">Choose an agent…</option>
          {groups.map(([id, group]) => (
            <optgroup key={id} label={many ? group.label : "Agents"}>
              {group.agents.map((agent) => (
                <option
                  key={agent.agent}
                  value={JSON.stringify({ kind: "agent", profile: agent.profile, host: agent.host, agent: agent.agent })}
                >
                  {agent.name}
                  {agent.needsYou ? " · needs you" : ""}
                  {agent.repository ? ` · ${agent.repository}` : ""}
                  {agent.local ? "" : ` · ${agent.hostName}`}
                </option>
              ))}
            </optgroup>
          ))}
          {state.folders.length > 0 && (
            <optgroup label="New agent in">
              {state.folders.map((folder) => (
                <option
                  key={`${folder.profile}:${folder.path}`}
                  value={JSON.stringify({ kind: "new", profile: folder.profile, cwd: folder.path })}
                >
                  {folder.name || folder.path}
                  {many ? ` · ${state.profiles.find((profile) => profile.id === folder.profile)?.label ?? ""}` : ""}
                </option>
              ))}
            </optgroup>
          )}
        </select>
      </label>
      <textarea
        rows={3}
        placeholder="What should the agent do with these? (optional)"
        value={message}
        onInput={(event) => onMessage((event.target as HTMLTextAreaElement).value)}
        onKeyDown={(event) => {
          if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
            event.preventDefault();
            onSend();
          }
        }}
      />
      <button type="submit" class="primary" disabled={!destination || state.sending}>
        {state.sending ? "Sending…" : label}
      </button>
    </form>
  );
}

render(<App />, document.getElementById("app")!);
