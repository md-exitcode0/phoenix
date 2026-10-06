import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

// The desktop UI is also served by the hidden Tauri/WebKit relay, so keep the
// terminal renderer as a small browser bundle instead of importing Node code
// from the Electron preload.
globalThis.PhoenixTerminal = Object.freeze({ Terminal, FitAddon });
