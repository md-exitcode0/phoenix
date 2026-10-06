#!/usr/bin/env python3
"""Phoenix AT-SPI bridge: act on the user's real apps through accessibility.

AT-SPI is the Linux accessibility bus, equivalent in role to macOS
Accessibility APIs. It lets Phoenix enumerate a running app's UI elements and
perform widget actions such as pressing a button, switching a tab, or setting a
text field without moving the mouse or stealing keyboard focus.

Perception doctrine (2026-07-08 rewrite): ONE call answers MANY questions.
- `read <app>` dumps the window's visible text in document order — a whole
  verification checklist becomes one call + one read.
- `locate <app> <query>` and `locate-many <app> <json-array>` match on the
  element NAME **and its Text-interface content** (a bare `<span>5 games</span>`
  has an empty accessible name — its content only exists as text; matching
  names alone made half of every web page invisible).
- A miss is never a bare error: it returns the closest visible texts with
  their click points, so the next step is grounded instead of guessed.

Output is always a single JSON object on stdout.
"""
import difflib
import json
import sys

try:
    import gi

    gi.require_version("Atspi", "2.0")
    from gi.repository import Atspi
except Exception as exc:  # pragma: no cover - environment dependent
    print(
        json.dumps(
            {
                "ok": False,
                "error": f"AT-SPI bindings unavailable: {exc}",
                "hint": "Install: sudo apt install python3-gi gir1.2-atspi-2.0 (and enable: gsettings set org.gnome.desktop.interface toolkit-accessibility true)",
            }
        )
    )
    sys.exit(0)

INTERESTING_ROLES = {
    "push button",
    "button",
    "toggle button",
    "link",
    "entry",
    "text",
    "password text",
    "check box",
    "radio button",
    "menu item",
    "check menu item",
    "radio menu item",
    "page tab",
    "combo box",
    "list item",
    "table cell",
    "spin button",
    "slider",
}

# Roles whose name is announced but is not readable page content (window
# chrome, containers). Their CHILDREN are still walked.
CHROME_ROLES = {"frame", "window", "application", "scroll bar", "tool bar"}

# Walk bounds: web-content a11y trees are deep and wide; these caps keep a
# full-window collection under ~1s while still covering a normal page.
MAX_DEPTH = 12
MAX_NODES = 4000
MAX_TEXT_CHARS = 240


def find_app(name):
    desktop = Atspi.get_desktop(0)
    for i in range(desktop.get_child_count()):
        app = desktop.get_child_at_index(i)
        if app and (app.get_name() or "").lower() == name.lower():
            return app
    for i in range(desktop.get_child_count()):
        app = desktop.get_child_at_index(i)
        if app and name.lower() in (app.get_name() or "").lower():
            return app
    return None


def node_at_path(app, path):
    node = app
    if path:
        for part in path.split("/"):
            node = node.get_child_at_index(int(part))
            if node is None:
                raise ValueError(f"no element at path segment {part}")
    return node


def screen_bounds(node):
    """Screen-space pixel bounds of an element via the AT-SPI Component iface.

    Returns {x, y, width, height, cx, cy} where (cx, cy) is the on-screen
    center — the point the agent's MOUSE clicks. This is the "accessibility to
    SEE, mouse to ACT" bridge (Codex's model): the a11y tree gives a precise,
    layout-independent target so the mouse never guesses pixels. Returns None
    for offscreen/zero-size/unsupported elements.
    """
    try:
        comp = node.get_component_iface()
    except Exception:
        comp = None
    if not comp:
        return None
    try:
        # SCREEN coords = the same virtual-stage pixel space the cursor uses.
        extents = comp.get_extents(Atspi.CoordType.SCREEN)
    except Exception:
        return None
    x, y, w, h = extents.x, extents.y, extents.width, extents.height
    if w <= 0 or h <= 0:
        return None  # not laid out / hidden
    return {
        "x": x,
        "y": y,
        "width": w,
        "height": h,
        "cx": x + w // 2,
        "cy": y + h // 2,
    }


def is_showing(node):
    """SHOWING per the a11y state set; True when the state can't be read (the
    bounds check already filtered unmapped elements)."""
    try:
        states = node.get_state_set()
        return states.contains(Atspi.StateType.SHOWING)
    except Exception:
        return True


def text_content(node):
    """The element's Text-interface content, bounded. This is where static
    web text lives (spans, headings, status lines) — such elements have an
    EMPTY accessible name."""
    try:
        text_iface = node.get_text_iface()
    except Exception:
        return ""
    if not text_iface:
        return ""
    try:
        count = text_iface.get_character_count()
        if count <= 0:
            return ""
        raw = text_iface.get_text(0, min(count, MAX_TEXT_CHARS))
    except Exception:
        return ""
    # Embedded-object placeholders (U+FFFC) mark child widgets, not text.
    return raw.replace("￼", " ").strip()


def normalize(s):
    return " ".join((s or "").split()).casefold()


def actions_of(node):
    try:
        action_iface = node.get_action_iface()
    except Exception:
        action_iface = None
    if not action_iface:
        return None, []
    names = []
    for index in range(action_iface.get_n_actions()):
        names.append(action_name(action_iface, index))
    return action_iface, names


def action_name(action_iface, index):
    # PyGObject exposes Accessible.get_name() on the same object, but that is
    # the element label and takes no action index. Prefer the AT-SPI action API.
    try:
        return action_iface.get_action_name(index)
    except Exception:
        try:
            return action_iface.get_name(index)
        except TypeError:
            return str(index)


def collect_visible(app, want_actions=False):
    """One tree walk that everything else (read/locate/inspect) reuses.

    Returns a document-ordered list of visible elements:
    {id, role, name, text, bounds[, actions]} — `text` from the Text iface,
    both `name` and `text` capped.
    """
    out = []
    visited = [0]

    def walk(node, path, depth):
        if depth > MAX_DEPTH or visited[0] >= MAX_NODES:
            return
        try:
            count = node.get_child_count()
        except Exception:
            return
        for i in range(min(count, 100)):
            if visited[0] >= MAX_NODES:
                return
            visited[0] += 1
            try:
                child = node.get_child_at_index(i)
                if not child:
                    continue
                child_path = f"{path}/{i}" if path else str(i)
                role = child.get_role_name()
                name_text = (child.get_name() or "").strip()
                text = text_content(child)
                bounds = screen_bounds(child)
                # Include unlabeled INTERESTING_ROLES widgets too: an empty
                # text field with no label has no name AND no text but is
                # still a form target inspect must show.
                if bounds and is_showing(child) and (
                    name_text or text or role in INTERESTING_ROLES
                ):
                    entry = {
                        "id": child_path,
                        "role": role,
                        "name": name_text[:MAX_TEXT_CHARS],
                        "text": text,
                        "bounds": bounds,
                    }
                    if want_actions:
                        _, actions = actions_of(child)
                        if actions:
                            entry["actions"] = actions
                    out.append(entry)
                walk(child, child_path, depth + 1)
            except Exception:
                continue

    walk(app, "", 0)
    return out


def cmd_apps():
    desktop = Atspi.get_desktop(0)
    apps = []
    for i in range(desktop.get_child_count()):
        app = desktop.get_child_at_index(i)
        if app:
            apps.append({"name": app.get_name() or "", "windows": app.get_child_count()})
    return {"ok": True, "apps": apps}


def cmd_inspect(name, max_items):
    app = find_app(name)
    if not app:
        return {"ok": False, "error": f"no a11y application named {name!r}"}
    elements = collect_visible(app, want_actions=True)
    out = []
    # Everything collect_visible returned is shown: actionable widgets,
    # unlabeled form fields, AND static text (headings, counts, status
    # lines) — the old actions-only filter forced one locate call per
    # string (the 2026-07-08 Pixel run burned ~20 round trips on checks
    # one inspect should answer).
    for entry in elements:
        item = {"id": entry["id"], "role": entry["role"], "name": entry["name"][:80]}
        if entry["text"] and normalize(entry["text"]) != normalize(entry["name"]):
            item["text"] = entry["text"]
        if entry.get("actions"):
            item["actions"] = entry["actions"]
        item["bounds"] = entry["bounds"]
        out.append(item)
        if len(out) >= max_items:
            break
    return {"ok": True, "app": app.get_name(), "count": len(out), "elements": out}


def cmd_read(name, max_chars):
    """Visible text of the app, document order, deduped — the one-call
    perception primitive. A verification checklist is answered by reading
    THIS once, not by N locate calls."""
    app = find_app(name)
    if not app:
        return {"ok": False, "error": f"no a11y application named {name!r}"}
    elements = collect_visible(app)
    lines = []
    seen = set()
    chars = 0
    truncated = False
    for entry in elements:
        if entry["role"] in CHROME_ROLES:
            continue
        piece = entry["text"] or entry["name"]
        key = normalize(piece)
        if not key or key in seen:
            continue
        seen.add(key)
        # Role prefix only where it changes how the agent acts on the line.
        if entry["role"] in ("push button", "button", "link", "entry", "combo box"):
            line = f"[{entry['role']}] {piece}"
        else:
            line = piece
        if chars + len(line) > max_chars:
            truncated = True
            break
        lines.append(line)
        chars += len(line) + 1
    return {
        "ok": True,
        "app": app.get_name(),
        "lines": lines,
        "chars": chars,
        "truncated": truncated,
        "hint": "every line above is visible on screen right now; to click one, locate it for its (cx, cy)",
    }


def match_score(query_norm, entry):
    """3 exact · 2 whole-word · 1 substring, against name OR text."""
    best = 0
    for candidate in (normalize(entry["name"]), normalize(entry["text"])):
        if not candidate:
            continue
        if candidate == query_norm:
            best = max(best, 3)
        elif f" {query_norm} " in f" {candidate} ":
            best = max(best, 2)
        elif query_norm in candidate:
            best = max(best, 1)
    return best


def closest_texts(query_norm, elements, limit=5):
    """The nearest visible texts to a missed query, WITH click points — the
    difference between the agent guessing three phrasings blind and seeing
    the actual wording in the miss itself."""
    scored = []
    for entry in elements:
        for label in (entry["text"], entry["name"]):
            norm = normalize(label)
            if not norm:
                continue
            ratio = difflib.SequenceMatcher(None, query_norm, norm).ratio()
            # Shared words rescue long lines that ratio punishes.
            query_words = set(query_norm.split())
            overlap = len(query_words & set(norm.split())) / max(1, len(query_words))
            score = max(ratio, overlap)
            if score > 0.3:
                scored.append(
                    (
                        score,
                        {
                            "text": label[:80],
                            "role": entry["role"],
                            "cx": entry["bounds"]["cx"],
                            "cy": entry["bounds"]["cy"],
                        },
                    )
                )
            break  # score each element once, by its primary label
    scored.sort(key=lambda pair: -pair[0])
    unique = []
    seen = set()
    for _, item in scored:
        key = normalize(item["text"])
        if key in seen:
            continue
        seen.add(key)
        unique.append(item)
        if len(unique) >= limit:
            break
    return unique


def locate_one(query, elements):
    query_norm = normalize(query)
    matches = []
    for entry in elements:
        score = match_score(query_norm, entry)
        if score:
            matches.append((score, entry))
    if not matches:
        return {
            "ok": False,
            "query": query,
            "error": f"no visible element matching {query!r}",
            "closest": closest_texts(query_norm, elements),
        }
    # Highest score, then the smallest area = the most specific target.
    matches.sort(
        key=lambda pair: (
            -pair[0],
            pair[1]["bounds"]["width"] * pair[1]["bounds"]["height"],
        )
    )
    best = matches[0][1]
    return {
        "ok": True,
        "query": query,
        "cx": best["bounds"]["cx"],
        "cy": best["bounds"]["cy"],
        "matched": {
            "id": best["id"],
            "role": best["role"],
            "name": best["name"][:80],
            "text": best["text"][:80],
        },
        "candidates": len(matches),
    }


def cmd_locate(name, queries):
    """Resolve one or many queries in ONE walk. Single query keeps the
    original flat shape; multiple queries return per-query results plus a
    found/missing summary (a checklist in one call)."""
    app = find_app(name)
    if not app:
        return {"ok": False, "error": f"no a11y application named {name!r}"}
    elements = collect_visible(app)
    results = [locate_one(q, elements) for q in queries]
    if len(results) == 1:
        return results[0]
    found = [r["query"] for r in results if r["ok"]]
    missing = [r["query"] for r in results if not r["ok"]]
    return {
        "ok": True,
        "found": found,
        "missing": missing,
        "results": results,
    }


def cmd_act(name, path, action):
    app = find_app(name)
    if not app:
        return {"ok": False, "error": f"no a11y application named {name!r}"}
    node = node_at_path(app, path)
    action_iface, names = actions_of(node)
    if not action_iface:
        return {"ok": False, "error": "element exposes no actions"}
    action_index = next(
        (
            index
            for index in range(action_iface.get_n_actions())
            if action_name(action_iface, index) == action
        ),
        None,
    )
    if action_index is None:
        return {"ok": False, "error": f"action {action!r} not in {names}"}
    ok = action_iface.do_action(action_index)
    return {
        "ok": bool(ok),
        "did": action,
        "on": {"role": node.get_role_name(), "name": node.get_name() or ""},
    }


def cmd_settext(name, path, text):
    app = find_app(name)
    if not app:
        return {"ok": False, "error": f"no a11y application named {name!r}"}
    node = node_at_path(app, path)
    try:
        editable = node.get_editable_text_iface()
    except Exception:
        editable = None
    if not editable:
        return {"ok": False, "error": "element is not editable text"}
    editable.set_text_contents(text)
    return {"ok": True, "set": True, "chars": len(text)}


def cmd_activate(name, query):
    """Find and invoke a visible control in one tree walk.

    Native accessibility activation is effectively instant and does not move
    the user's pointer. When a toolkit exposes geometry but no action, return
    the grounded point so Rust can use the visible Phoenix cursor fallback.
    """
    app = find_app(name)
    if not app:
        return {"ok": False, "error": f"no a11y application named {name!r}"}
    result = locate_one(query, collect_visible(app, want_actions=True))
    if not result.get("ok"):
        return result
    node = node_at_path(app, result["matched"]["id"])
    action_iface, names = actions_of(node)
    preferred = ("click", "press", "activate", "jump", "open")
    if action_iface:
        normalized = [normalize(item) for item in names]
        index = next(
            (normalized.index(candidate) for candidate in preferred if candidate in normalized),
            0 if names else None,
        )
        if index is not None and action_iface.do_action(index):
            return {
                "ok": True,
                "acted": True,
                "did": names[index],
                "matched": result["matched"],
            }
    return {
        "ok": True,
        "acted": False,
        "cx": result["cx"],
        "cy": result["cy"],
        "matched": result["matched"],
        "actions": names,
    }


def cmd_settext_query(name, query, text):
    app = find_app(name)
    if not app:
        return {"ok": False, "error": f"no a11y application named {name!r}"}
    result = locate_one(query, collect_visible(app))
    if not result.get("ok"):
        return result
    node = node_at_path(app, result["matched"]["id"])
    try:
        editable = node.get_editable_text_iface()
    except Exception:
        editable = None
    if not editable:
        return {
            "ok": True,
            "set": False,
            "cx": result["cx"],
            "cy": result["cy"],
            "matched": result["matched"],
        }
    editable.set_text_contents(text)
    return {
        "ok": True,
        "set": True,
        "chars": len(text),
        "matched": result["matched"],
    }


def dispatch(args):
    if not args:
        return {
            "ok": False,
            "error": "usage: apps | read | inspect | locate | locate-many | activate | settext-query | act | settext",
        }
    cmd = args[0]
    if cmd == "apps":
        return cmd_apps()
    if cmd == "inspect":
        max_items = int(args[args.index("--max") + 1]) if "--max" in args else 250
        return cmd_inspect(args[1], max_items)
    if cmd == "read":
        max_chars = int(args[args.index("--max-chars") + 1]) if "--max-chars" in args else 6000
        return cmd_read(args[1], max_chars)
    if cmd == "locate":
        return cmd_locate(args[1], [" ".join(args[2:])])
    if cmd == "locate-many":
        queries = json.loads(args[2])
        if not isinstance(queries, list) or not all(isinstance(q, str) for q in queries):
            return {"ok": False, "error": "locate-many needs a JSON array of strings"}
        return cmd_locate(args[1], queries)
    if cmd == "activate":
        return cmd_activate(args[1], " ".join(args[2:]))
    if cmd == "settext-query":
        return cmd_settext_query(args[1], args[2], " ".join(args[3:]))
    if cmd == "act":
        return cmd_act(args[1], args[2], args[3])
    if cmd == "settext":
        return cmd_settext(args[1], args[2], " ".join(args[3:]))
    return {"ok": False, "error": f"unknown subcommand {cmd!r}"}


def serve():
    """Long-lived line-delimited bridge; one AT-SPI process per Phoenix.

    Requests are JSON arrays of argv-like strings and responses are one JSON
    object per line. Bounds prevent a malformed local caller from growing the
    process indefinitely.
    """
    for line in sys.stdin:
        if len(line) > 262144:
            print(json.dumps({"ok": False, "error": "request is too large"}), flush=True)
            continue
        try:
            args = json.loads(line)
            if not isinstance(args, list) or not all(isinstance(arg, str) for arg in args):
                raise ValueError("request must be a JSON array of strings")
            result = dispatch(args)
        except Exception as exc:
            result = {"ok": False, "error": f"{type(exc).__name__}: {exc}"}
        print(json.dumps(result), flush=True)


def main():
    args = sys.argv[1:]
    Atspi.init()
    try:
        if args == ["serve"]:
            serve()
            return None
        return dispatch(args)
    finally:
        try:
            Atspi.exit()
        except Exception:
            pass


if __name__ == "__main__":
    try:
        result = main()
        if result is not None:
            print(json.dumps(result))
    except Exception as exc:
        print(json.dumps({"ok": False, "error": f"{type(exc).__name__}: {exc}"}))
