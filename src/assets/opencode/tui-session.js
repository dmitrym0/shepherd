// Installed by shepherd. Managed file: reinstalling overwrites it.
//
// Ported from herdr (https://github.com/ogulcancelik/herdr) integration
// version 11, AGPL-3.0-or-later, copyright Ogulcan Celik and contributors.
// SHEPHERD_INTEGRATION_ID=opencode-tui
// SHEPHERD_INTEGRATION_VERSION=1

import net from "node:net";

const SOURCE = "shepherd:opencode";
const AGENT = "opencode";
const ROUTE_POLL_INTERVAL_MS = 100;
const SELECTION_RETRY_DELAYS_MS = [100, 400, 1_000];
// After the ladder, keep re-reporting slowly forever. The server holds
// identity in memory only, so a restart forgets it; nothing else would tell
// this plugin that happened. Without the heartbeat an idle session stays
// anonymous until the user touches it — which is exactly the session they
// are most likely to be looking at (shepherd git-bug 69681aa).
const SELECTION_HEARTBEAT_MS = 30_000;

function requestOnce(sessionID) {
  const agentId = process.env.SHEPHERD_AGENT_ID;
  const socketPath = process.env.SHEPHERD_SOCKET_PATH;
  if (!agentId || !socketPath) {
    return Promise.resolve();
  }

  const socketEndpoint =
    process.platform === "win32" ? `\\\\.\\pipe\\${socketPath}` : socketPath;
  const request = {
    id: `${SOURCE}:tui:${Date.now()}:${Math.floor(Math.random() * 1_000_000)
      .toString()
      .padStart(6, "0")}`,
    method: "agent.report_session",
    params: {
      agent_id: agentId,
      source: SOURCE,
      agent: AGENT,
      agent_session_id: sessionID,
    },
  };

  return new Promise((resolve) => {
    const client = net.createConnection(socketEndpoint, () => {
      client.write(`${JSON.stringify(request)}\n`);
    });
    const finish = () => {
      client.destroy();
      resolve();
    };

    client.setTimeout(500, finish);
    client.on("data", finish);
    client.on("error", finish);
    client.on("end", finish);
    client.on("close", resolve);
  });
}

export default {
  id: "shep.opencode.session-selection",
  tui: async (api) => {
    if (
      process.env.SHEPHERD_ENV !== "1" ||
      !process.env.SHEPHERD_SOCKET_PATH ||
      !process.env.SHEPHERD_AGENT_ID
    ) {
      return;
    }

    let selectedSessionID;
    let retryIndex = 0;
    let nextReportAt = 0;
    let reportPending = false;
    const syncSelectedSession = async () => {
      const route = api.route.current;
      const sessionID = route?.name === "session" ? route.params?.sessionID : undefined;
      const session =
        typeof sessionID === "string" && sessionID
          ? api.state.session.get(sessionID)
          : undefined;
      if (!session || session.parentID) {
        selectedSessionID = undefined;
        retryIndex = 0;
        nextReportAt = 0;
        return;
      }
      if (sessionID !== selectedSessionID) {
        selectedSessionID = sessionID;
        retryIndex = 0;
        nextReportAt = 0;
      }
      if (reportPending || Date.now() < nextReportAt) {
        return;
      }

      const reportingSessionID = sessionID;
      reportPending = true;
      try {
        await requestOnce(reportingSessionID);
      } catch {
        // Best-effort reporting retries below while the selected route remains active.
      } finally {
        reportPending = false;
      }
      if (selectedSessionID !== reportingSessionID) {
        retryIndex = 0;
        nextReportAt = 0;
        return;
      }
      const retryDelay = SELECTION_RETRY_DELAYS_MS[retryIndex];
      retryIndex += 1;
      nextReportAt = Date.now() + (retryDelay ?? SELECTION_HEARTBEAT_MS);
    };

    await syncSelectedSession();
    const routePoll = setInterval(() => void syncSelectedSession(), ROUTE_POLL_INTERVAL_MS);
    api.lifecycle.onDispose(() => clearInterval(routePoll));
  },
};
