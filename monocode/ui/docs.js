"use strict";
// Phoenix user guide, shown in Settings → Settings guide. Plain data: each tab
// holds short articles; `open` names the Settings section an article links to
// and `keys` adds search words that are not in the text itself.
window.PhoenixDocs = [
  { id: "start", title: "Getting started", articles: [
    { title: "What Phoenix is", keys: "overview intro company coworkers team", body: `
      <p>Phoenix is a small company of AI coworkers that runs on your computer. <b>Phoenix</b> is the Chief of Staff: tell it what you want and it plans the work, hands parts to the right coworker, checks the results and answers you. Each coworker has its own chat, memory, browser and tools.</p>
      <p>You can talk to anyone directly from the sidebar, or bring several of them together in a <b>group</b>.</p>` },
    { title: "First steps", keys: "setup onboarding begin provider key", open: "Models & Providers", body: `
      <ol><li><b>Connect a model provider</b> in Models &amp; Providers (for example OpenAI, Anthropic or Groq). Coworkers cannot think without one.</li>
      <li><b>Pick a workspace folder</b> above the composer. That is where coworkers read and write files.</li>
      <li><b>Say what you want</b> to Phoenix in plain words. Follow along in the chat and in the panel on the right.</li></ol>` },
    { title: "Keyboard shortcuts", keys: "hotkeys keys ctrl cmd", body: `
      <table class="docs-keys"><tr><td><kbd>Ctrl/⌘ K</kbd></td><td>Search your company: coworkers, groups, chats, settings</td></tr>
      <tr><td><kbd>Ctrl B</kbd></td><td>Show or hide the workspace panel (Review, Browser, Desktop)</td></tr>
      <tr><td><kbd>Ctrl J</kbd></td><td>Show or hide the terminal</td></tr>
      <tr><td><kbd>Ctrl \\</kbd></td><td>Collapse or open the left sidebar</td></tr>
      <tr><td><kbd>Enter</kbd> / <kbd>Shift Enter</kbd></td><td>Send / new line (Settings → Composer can switch sending to <kbd>Ctrl/⌘ Enter</kbd>)</td></tr>
      <tr><td><kbd>Ctrl +</kbd> / <kbd>Ctrl -</kbd> / <kbd>Ctrl 0</kbd></td><td>Make the whole interface larger, smaller, or back to normal (Ctrl + scroll works too)</td></tr>
      <tr><td><kbd>Esc</kbd></td><td>Close a menu, popover or dialog</td></tr></table>` },
    { title: "Light and dark theme", keys: "theme dark mode light appearance", open: "Appearance", body: `<p>Click the sun/moon button at the bottom of the sidebar to switch. Settings → Appearance has Light, Dark and Auto (follows your system), plus corners, density and colours.</p>` },
  ]},
  { id: "chat", title: "Chatting", articles: [
    { title: "The composer", keys: "message input send prompt", body: `
      <p>Type your request and press <kbd>Enter</kbd>. While a coworker is working you can keep typing: new messages queue up and run next. The round button turns into <b>Stop</b> while it works.</p>
      <p>Use <b>@name</b> to pull a specific coworker in, and the <b>+</b> button to attach files, folders or images.</p>` },
    { title: "Access: Talk, Workspace, Full access", keys: "permission permissions access mode yolo safe", open: "Permissions", body: `
      <ul><li><b>Talk</b>: conversation only. No files, no commands.</li>
      <li><b>Workspace</b>: company tools inside your chosen folder.</li>
      <li><b>Full access</b>: the folder stays the default, but coworkers may also work anywhere else on the computer.</li></ul>
      <p>The choice sticks per coworker. Sensitive actions (sending, deleting, buying) still ask you first.</p>` },
    { title: "Model, context and reasoning", keys: "model selector context window reasoning effort thinking", open: "Models & Providers", body: `
      <p>Next to the <b>+</b> button: the <b>model</b> picker (only providers you connected), <b>Context</b> (how much the coworker can keep in mind at once) and <b>Reasoning</b> (how hard it thinks). Click one to open its slider; click it again to close. The counter shows how full the context is.</p>` },
    { title: "Approvals and questions", keys: "approve deny allow always question ask", body: `
      <p>When a coworker needs a decision it shows a card in the chat. <b>Allow once</b> approves this action, <b>Always allow</b> remembers it for this coworker, <b>Deny</b> stops it. Questions offer choices and a free-text answer. Unsent answers to ordinary questions are saved automatically on this device.</p>` },
    { title: "To-dos and progress", keys: "todo tasks plan progress checklist", body: `<p>For larger jobs the coworker keeps a to-do list above the composer. Each item shows its status, and bigger items show a progress estimate. Expand it to see the whole plan.</p>` },
    { title: "Saving prompts for later", keys: "stash drafts saved prompts", body: `<p>The <b>stash</b> button (the box icon next to the mic) saves what you typed and clears the composer. Click it with an empty composer, or right-click it, to open your saved prompts and reuse one.</p>` },
    { title: "Voice input", keys: "microphone mic speak dictate speech", open: "Voice", body: `<p>Click the mic to dictate. It needs a speech-to-text model: in Models &amp; Providers → System jobs → <b>Speech to text</b>, choose Groq → <b>Whisper Large v3 Turbo</b> (free with a Groq key).</p>` },
    { title: "Finding earlier messages", keys: "history prompt rail scroll jump previous", body: `<p>Move your mouse to the right edge of the chat: a rail with your earlier prompts appears. Hover to preview, click to jump there.</p>` },
    { title: "Copy and delete", keys: "copy delete remove answer turn", body: `<p>Under each answer: when it replied, then <b>copy</b>, then <b>delete</b>. Deleting an answer removes the whole turn; the bin on your own message removes that prompt and its answer.</p>` },
  ]},
  { id: "team", title: "Coworkers & groups", articles: [
    { title: "Creating a coworker", keys: "new agent hire create", open: "Agents & Groups", body: `<p>Use <b>Create an agent</b> at the top of the sidebar. Give it a name and a job; Phoenix sets up its tools, memory and browser. You can also ask Phoenix to “hire someone for …”.</p>` },
    { title: "Groups", keys: "group team room everyone mention", open: "Agents & Groups", body: `<p><b>Create a group</b> to put several coworkers in one chat. Mention one with <b>@name</b>, or everyone at once. Each member still keeps its own private memory.</p>` },
    { title: "Row menu: pin, rename, avatar, archive, delete", keys: "right click menu pin archive delete rename avatar", body: `
      <p>Right-click any coworker or group in the sidebar (or use its <b>…</b> button) to pin it, edit it, change its avatar, archive it or delete it.</p>
      <p>Deleted coworkers stay recoverable for <b>30 days</b>; they disappear from pickers and settings right away.</p>` },
    { title: "Avatars", keys: "avatar icon gradient image picture", body: `<p>Pick one of two animated marks and one of ten gradients (or your own), or upload an image. The mark animates while that coworker is working.</p>` },
    { title: "Per-coworker settings", keys: "applies to scope override per agent", body: `<p>At the top of Settings, <b>Applies to</b> switches between company defaults and one coworker or group, so you can give one coworker a different model, access or behaviour.</p>` },
  ]},
  { id: "panel", title: "Workspace panel", articles: [
    { title: "Review: files and changes", keys: "review diff changes files tree edits", body: `
      <p>The <b>Review</b> tab shows exactly what a coworker changed: each file's diff on the left, the workspace file tree on the right. Filter the tree with the search box. Drag the line between them to resize.</p>` },
    { title: "The coworker's browser", keys: "browser tabs chrome web login cookies extensions", open: "Browser & Accounts", body: `
      <p>Each coworker has its own private browser that stays signed in. Its tabs appear in the panel; nothing closes them automatically. Click a tab to watch or take over, <b>+</b> opens a new tab.</p>
      <p>When a site needs your login, the coworker asks you to sign in yourself in that browser. It never types your passwords.</p>` },
    { title: "The coworker's desktop", keys: "desktop computer use screen fullscreen", body: `<p>For desktop apps a coworker gets its own private desktop. The <b>Desktop</b> tab shows it live; use the full-screen button to watch it large.</p>` },
    { title: "Terminal", keys: "terminal shell console bottom panel", body: `<p><kbd>Ctrl J</kbd> opens a terminal in your workspace folder at the bottom of the window.</p>` },
    { title: "Teach agent", keys: "teach demonstrate record workflow show", open: "Workflows & Routines", body: `<p><b>Teach agent</b> (top right of a chat) opens the browser in teaching mode: do the task once yourself and the coworker saves it as a reusable workflow.</p>` },
  ]},
  { id: "models", title: "Models & accounts", articles: [
    { title: "Providers and accounts", keys: "provider account api key login openai anthropic groq add", open: "Models & Providers", body: `<p>Models &amp; Providers → <b>Providers</b> lists every service you can connect. Add an API key or sign in; a provider can hold several accounts.</p>` },
    { title: "Who uses which model", keys: "lanes routes coworkers system jobs model assignment", open: "Models & Providers", body: `
      <p><b>Coworkers</b>: the model each coworker thinks with. <b>System jobs</b>: models for background work, image understanding and generation, memory summaries and voice (speech to text, spoken replies, realtime voice). Voice rows only list voice models.</p>` },
    { title: "Backup accounts and providers", keys: "fallback backup quota limit rate limit rotate", open: "Models & Providers", body: `<p>When an account hits its limit or fails, Phoenix moves to the next account or backup provider automatically and comes back later. Set the order under <b>Advanced backup providers</b>.</p>` },
    { title: "Usage and cost", keys: "tokens cost spend usage billing", open: "Usage & cost", body: `<p>Usage &amp; cost shows tokens, cache use and time per model and per day, as reported by each provider.</p>` },
    { title: "Speech to text (free)", keys: "whisper groq transcribe stt voice audio", open: "Models & Providers", body: `
      <ol><li>Get a free API key at console.groq.com.</li><li>Add it under Providers → Groq.</li><li>System jobs → Speech to text: Groq → <b>Whisper Large v3 Turbo</b>.</li></ol>
      <p>That powers the mic, Telegram/Discord voice messages and the coworkers' <b>transcribe_audio</b> tool for recordings and videos (up to 25 MB per file).</p>` },
  ]},
  { id: "powers", title: "Powers", articles: [
    { title: "Skills", keys: "skills playbooks install library", open: "Skills", body: `<p>Skills are proven playbooks coworkers load before doing a kind of task (a design system, a deploy routine…). Browse and install them in Settings → Skills.</p>` },
    { title: "Apps (Composio)", keys: "composio apps gmail slack github integrations oauth", open: "Composio", body: `<p>Connect everyday apps (email, calendar, GitHub, Slack…) through Composio so coworkers can act in them. Every outside action that sends or deletes still asks you first.</p>` },
    { title: "MCP servers", keys: "mcp server tools protocol", open: "MCP", body: `<p>Add local or remote MCP servers to give coworkers extra tools, and optionally route a server to one coworker.</p>` },
    { title: "Remote runners", keys: "remote runner machine server", open: "Remote runners", body: `<p>Let coworkers run work on another machine you control.</p>` },
  ]},
  { id: "chatapps", title: "Telegram & Discord", articles: [
    { title: "Set up Telegram", keys: "telegram bot botfather token chat id phone", open: "Chat apps", body: `
      <ol><li>Settings → Chat apps → <b>Set up Telegram</b>.</li>
      <li>In Telegram open <b>@BotFather</b>, send <code>/newbot</code>, pick a name and a username ending in “bot”. Paste the token it gives you and click <b>Check token</b>.</li>
      <li>Send your new bot any message. Phoenix finds your chat and your user ID by itself; you never type IDs.</li>
      <li>Choose which coworker or group answers, then <b>Connect Telegram</b>.</li></ol>` },
    { title: "Telegram groups", keys: "telegram group privacy admin setprivacy", body: `<p>Add the bot to the group and send a message that mentions it (“@yourbot hi”). By default a bot only sees messages that mention or reply to it; to let it read everything, make it a group admin or send <code>/setprivacy</code> → Disable to @BotFather.</p>` },
    { title: "Set up Discord", keys: "discord bot server channel developer portal intent invite", open: "Chat apps", body: `
      <ol><li>Settings → Chat apps → <b>Set up Discord</b>.</li>
      <li>In the Discord Developer Portal: New Application → Bot → turn on <b>Message Content Intent</b> → Reset Token. Paste the token and click <b>Check token</b>.</li>
      <li>Click <b>Invite the bot</b>, add it to your server, then pick the server and channel.</li>
      <li>Say hi in that channel; untick anyone who should not give instructions. Choose who answers and <b>Connect Discord</b>.</li></ol>` },
    { title: "What shows up where", keys: "mirror sync typing voice message transcript", body: `
      <ul><li>Your chat messages appear in that coworker's chat in Phoenix, marked “via Telegram · your name”.</li>
      <li>What you write to that coworker inside Phoenix is mirrored to the chat with its answer.</li>
      <li>The chat shows <b>typing…</b> while the coworker works.</li>
      <li>Voice messages are transcribed (needs Speech to text, see Models &amp; accounts).</li></ul>
      <p>Phoenix must be running to receive messages. It reconnects saved chats after a restart unless you pressed Disconnect.</p>` },
  ]},
  { id: "auto", title: "Memory & automation", articles: [
    { title: "Memory", keys: "memory remember recall knowledge forget", open: "Memory", body: `<p>Coworkers remember facts, preferences and decisions across chats. Each coworker has private memory; Phoenix keeps team-wide notes. Tell a coworker “remember …” or “forget …”, and manage it in Settings → Memory.</p>` },
    { title: "Workflows and routines", keys: "workflow routine taught reuse automate", open: "Workflows & Routines", body: `<p>Taught workflows are saved step lists coworkers can reuse. Settings → Workflows &amp; Routines shows each one's success rate, who owns it and who may use it (one coworker, a group or the whole company).</p>` },
    { title: "Schedules", keys: "schedule cron recurring daily weekly timer reminder", open: "Schedules", body: `<p>Ask a coworker to do something every day, every week, every few minutes or once later, or create it in Settings → Schedules. Each run continues that coworker's chat.</p>` },
    { title: "Notifications", keys: "notifications alerts sound", open: "Notifications", body: `<p>Choose what Phoenix tells you about: things that need your attention, finished work and coworker updates, with or without sound, as system notifications and as badges in the sidebar.</p>` },
  ]},
  { id: "help", title: "Troubleshooting", articles: [
    { title: "A coworker doesn't answer", keys: "stuck not responding error credits quota failed", open: "Models & Providers", body: `<p>Most often the provider account is out of credit or its key expired. Check Models &amp; Providers: a failing account shows an error, and adding a second account or a backup provider lets Phoenix switch automatically.</p>` },
    { title: "Telegram or Discord gets no reply", keys: "telegram discord not working no reply offline", open: "Chat apps", body: `<p>Phoenix must be running. In Settings → Chat apps the connection should say <b>Connected</b>; if not, open <b>Manage</b> → Connect. In groups, check the bot's privacy mode (see Telegram &amp; Discord).</p>` },
    { title: "Voice messages are not understood", keys: "voice audio transcription whisper failed", open: "Models & Providers", body: `<p>Set Speech to text to a Whisper model (Groq → Whisper Large v3 Turbo) and make sure a Groq key is added. Recordings over 20 MB (Telegram) or 25 MB are skipped with a note.</p>` },
    { title: "The browser tab disappeared", keys: "browser tab missing closed gone", body: `<p>Coworker browsers are never closed automatically. Open that coworker's chat: its open tabs appear again in the panel; click one to reconnect.</p>` },
    { title: "Everything feels slow", keys: "slow lag laggy performance", body: `<p>Most waiting is the model provider thinking. Lower <b>Reasoning</b> for simple tasks, choose a faster model, or reduce effects in Appearance.</p>` },
  ]},
];
