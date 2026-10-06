# Telegram and Discord channels

This first implementation runs as a local worker alongside the Phoenix gateway. It accepts text from specified users in one specified conversation and returns the chosen agent's final reply and images embedded in that reply. It does not forward tool calls, reasoning, worker returns, or incidental screenshots. Desktop setup is available; live-provider acceptance is still pending.

## Desktop setup

Open **Connections & skills → Channels → Add channel**. Name the connection, select an agent, and enter the exact chat/channel and allowed user IDs. Save, then choose **Manage** to paste the bot token and connect. Choose **Remember login** to store the token in your initialized encrypted vault and reconnect without re-entering it. You can also connect without saving. Tokens never enter connection JSON. A saved login can be replaced or forgotten under **Manage → Replace or forget login** while disconnected.

**Test login** checks the bot credentials without sending a message. **Refresh status** reads current worker/delivery state. **Manage** includes disconnect, rename/access edits, removal and interrupted-turn/reply recovery. Only workers started by this desktop can be disconnected there; CLI workers must be stopped in their terminal. Keep Phoenix running; closing the desktop disconnects its channel workers while agent work remains in Phoenix.

## CLI setup


Save a private JSON connection file, for example `~/.phoenix/channels/personal.json`:

```json
{
  "id": "personal",
  "name": "Phoenix on Telegram",
  "platform": "telegram",
  "conversation_id": "123456789",
  "allowed_user_ids": ["123456789"],
  "agent_id": "phoenix",
  "session_id": "YOUR_AGENT_CANONICAL_SESSION_ID",
  "workspace": "/absolute/path/to/your/workspace",
  "token_env": "PHOENIX_TELEGRAM_BOT_TOKEN",
  "enabled": true
}
```

Use the chosen agent's existing canonical session. Use a bound saved vault login, supply the token on stdin with `--token-stdin`, or use the named environment variable. Do not put it in command arguments, connection JSON, or the conversation. `channels remember-token CONNECTION.json` reads a token from stdin into the initialized vault; `channels forget-token CONNECTION.json` removes that saved login. Both require the worker to be disconnected. Start Phoenix normally, then:

```sh
phoenix channels check ~/.phoenix/channels/personal.json
phoenix channels run ~/.phoenix/channels/personal.json
phoenix channels status ~/.phoenix/channels/personal.json
```

For Discord use `"platform": "discord"`, the exact channel ID and allowed Discord user IDs, and a bot token. The bot needs access to that channel, Read Message History, Send Messages, Attach Files, and Message Content access where Discord requires it. The initial adapter polls the selected channel every two seconds; it does not require a public inbound HTTP endpoint. Telegram establishes its initial history boundary with an immediate poll, then uses 20-second long polling independently of outgoing replies. It needs its own bot without another active receiver/webhook. One Telegram bot can have only one active Phoenix connection worker.

A new connection establishes a boundary and ignores historical messages. Subsequent cursors and admitted messages are saved before advancing the provider cursor. Only configured users can create turns. Channel turns use Workspace permission; approval-dependent work remains reviewable in Phoenix. Bots/webhooks and Telegram message edits never submit turns.

## Replies and recovery

Long replies split without dropping text or breaking Unicode characters. Discord mentions are disabled. Explicit final-answer PNG/JPEG embeds inside the configured workspace are copied into the private outbox before upload, so later file edits cannot change a queued screenshot. Maximum 10 images per answer, 8 MiB per image. Images outside that workspace show a short “open Phoenix” notice; remote URLs are not fetched by the worker.

Replies remain in a durable outbox. Rate-limited sends respect the returned delay. A lost HTTP response has an unknown delivery outcome: the worker retains it as `uncertain` and stops later reply parts instead of sending duplicates. Disconnect the worker and inspect the destination before resolving it:

```sh
# Use the delivery ID shown by status. Default means you confirmed it arrived.
phoenix channels resolve-delivery CONNECTION.json DELIVERY_ID
# Only if you checked that the reply did not arrive:
phoenix channels resolve-delivery CONNECTION.json DELIVERY_ID --retry
```

Ctrl-C disconnects the channel. It does not silently cancel the agent's work in Phoenix. Interrupted turns remain `needs_review` and are not resubmitted on restart. After handling an interrupted turn in Phoenix, use `phoenix channels acknowledge-turn CONNECTION.json TURN_ID` (from status) to unblock later messages without resubmitting that turn. Automatic reconciliation and ask/approval responses are unfinished. The desktop recovery UI exposes the same explicit resolution choices. Queued later turns stop behind an uncertain running turn. Changing destination/agent/workspace requires a new connection ID, keeping stored replies bound to their original destination.

## Current verification

Local HTTP fixtures test Telegram and Discord request bodies, bootstrap behavior, Unicode splitting, mention suppression, multipart image uploads and rate-limit delays. Durable-state tests check duplicate admission and uncertain delivery recovery. A Unix-socket gateway test checks that noisy tool and worker events never cross into the reply and that the final reply waits for two background workers to settle. Managed-connection tests cover edits, locks, destination binding and removal after recovery. Fifteen channel tests and nine vault regression tests pass, including saved-login rotation, destination binding and scoped deletion. The CLI checks, gateway and desktop builds, and dark/light UI fixtures also pass. These fixtures do not prove live provider delivery.

No live Telegram/Discord send is implied by these tests. Before marking channels complete, configure real test destinations, verify two-way replies and screenshots, exercise reconnect/recovery, verify desktop setup against the real providers, and verify the background final-answer path end to end.

References: [Telegram Bot API](https://core.telegram.org/bots/api), [Discord messages](https://docs.discord.com/developers/resources/message), [Discord Gateway](https://docs.discord.com/developers/events/gateway).


## Responsive receive loop

Incoming polling now runs as one cancellable request alongside the worker loop. An awaiting poll cannot hold up a finished answer. The worker admits and saves each batch before requesting its next cursor; a second receive request cannot overlap it. Disconnect cancels the outstanding poll. Outgoing sends keep their existing durable/uncertain-delivery handling.

Telegram uses an immediate initial history check before the worker reports connected, then 20-second long polling. This avoids treating a first newly received message as startup history and reduces idle polling requests. Discord retains its two-second interval. HTTP fixtures cover delivering a reply while polling is held open, keeping the cursor stable until its result is consumed, and canceling the pending request on disconnect. Real provider acceptance remains pending.

Reference: [Telegram update offsets and confirmations](https://core.telegram.org/bots/api#getupdates). The cursor still advances only after local admission is saved.

Responsive polling verification: all 15 channel tests passed, including the CLI final-answer boundary and saved-login management. Gateway build passed.
