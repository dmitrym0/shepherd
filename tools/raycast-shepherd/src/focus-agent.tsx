import {
  Action,
  ActionPanel,
  Color,
  List,
  Toast,
  closeMainWindow,
  getPreferenceValues,
  showHUD,
  showToast,
} from "@raycast/api";
import { execFile } from "child_process";
import { homedir } from "os";
import { useEffect, useState } from "react";

type Agent = {
  agent_id: string;
  name?: string;
  agent?: string;
  agent_status: string;
  blocked_reason?: string;
  cwd?: string;
  metadata?: Record<string, string>;
  terminal?: { app: string; session_id: string };
};

const ORDER: Record<string, number> = { blocked: 0, done: 1, working: 2, idle: 3 };
const TAG_COLOR: Record<string, Color> = {
  blocked: Color.Red,
  done: Color.Orange,
  working: Color.Blue,
  idle: Color.SecondaryText,
};

export default function Command() {
  const [agents, setAgents] = useState<Agent[]>();
  const [error, setError] = useState<string>();
  const [jiraBase, setJiraBase] = useState<string>();

  useEffect(() => {
    fetch("http://localhost:4650/agents")
      .then((r) => r.json() as Promise<Agent[]>)
      .then((a) =>
        setAgents(a.sort((x, y) => (ORDER[x.agent_status] ?? 9) - (ORDER[y.agent_status] ?? 9))),
      )
      .catch(() => setError("shepherd server not running"));
    fetch("http://localhost:4650/config")
      .then((r) => r.json() as Promise<{ jira_base_url?: string }>)
      .then((c) => setJiraBase(c.jira_base_url))
      .catch(() => {});
  }, []);

  return (
    <List isLoading={!agents && !error} searchBarPlaceholder="Filter agents…">
      {error ? (
        <List.EmptyView title={error} description="Start one with: shep run claude" />
      ) : (
        agents?.map((a) => (
          <List.Item
            key={a.agent_id}
            title={a.name ?? a.agent ?? a.agent_id}
            subtitle={a.metadata?.description ?? a.blocked_reason ?? a.cwd}
            keywords={Object.entries(a.metadata ?? {}).flatMap(([k, v]) => [k, v, `${k}=${v}`])}
            accessories={[
              ...(a.metadata?.jira ? [{ text: a.metadata.jira }] : []),
              { tag: { value: a.agent_status, color: TAG_COLOR[a.agent_status] ?? Color.SecondaryText } },
              ...(a.terminal ? [] : [{ text: "no terminal" }]),
            ]}
            actions={
              <ActionPanel>
                <Action title="Focus Terminal" onAction={() => focus(a)} />
                {a.metadata?.jira && jiraBase && (
                  <Action.OpenInBrowser
                    title="Open Jira Ticket"
                    url={`${jiraBase.replace(/\/+$/, "")}/browse/${a.metadata.jira}`}
                  />
                )}
                {a.metadata?.url && <Action.OpenInBrowser title="Open URL" url={a.metadata.url} />}
              </ActionPanel>
            }
          />
        ))
      )}
    </List>
  );
}

async function focus(a: Agent) {
  if (!a.terminal) {
    await showToast({ style: Toast.Style.Failure, title: "No recorded terminal for this agent" });
    return;
  }
  const pref = getPreferenceValues<{ focusScript: string }>();
  const script = pref.focusScript.replace(/^~/, homedir());
  await closeMainWindow();
  execFile(script, [a.terminal.session_id], (err, _stdout, stderr) => {
    void showHUD(err ? `Focus failed: ${stderr || err.message}` : "Focused");
  });
}
