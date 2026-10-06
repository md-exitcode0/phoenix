//! Secret-safe observation of human input in the compositor-backed browser.
//!
//! Native X11 input never crosses Canvas, so the old `TeachWorkflow::Interact`
//! path cannot see it. This observer runs in a named CDP isolated world, where
//! website JavaScript cannot call or replace its binding, and emits only
//! semantic clicks/selects/navigation plus one complete field value on
//! blur/change/Enter. Password/OTP/credential values are replaced with an
//! empty parameter marker in JavaScript before they cross the CDP boundary.

use super::*;
use headless_chrome::protocol::cdp::Page;
use std::sync::mpsc::{sync_channel, TrySendError};
use zeroize::Zeroize;

const OBSERVER_WORLD: &str = "phoenix_teaching_v1";
const MAX_OBSERVED_EVENT_BYTES: usize = 96 * 1024;
const MAX_OBSERVED_URL_BYTES: usize = 16 * 1024;
const MAX_OBSERVED_TARGET_BYTES: usize = 4 * 1024;

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ObservedEvent {
    Navigate {
        url: String,
        #[serde(default)]
        linked_click: bool,
    },
    Click {
        url: String,
        target: BrowserSemanticTarget,
    },
    Type {
        url: String,
        target: BrowserSemanticTarget,
        #[serde(default)]
        value: String,
        #[serde(default)]
        sensitive: bool,
    },
    Select {
        url: String,
        target: BrowserSemanticTarget,
        option: String,
    },
}

fn validate_text(value: &str, max: usize, label: &str) -> Result<()> {
    anyhow::ensure!(
        value.len() <= max && !value.contains('\0'),
        "native teaching {label} is too large or invalid"
    );
    Ok(())
}

fn validate_target(target: &BrowserSemanticTarget) -> Result<()> {
    validate_text(&target.tag, 64, "target tag")?;
    anyhow::ensure!(!target.tag.is_empty(), "native teaching target has no tag");
    for value in [
        &target.role,
        &target.input_type,
        &target.autocomplete,
        &target.name,
        &target.element_id,
        &target.label,
        &target.text,
    ] {
        validate_text(value, 512, "target field")?;
    }
    validate_text(
        &target.selector,
        MAX_OBSERVED_TARGET_BYTES,
        "target selector",
    )
}

fn persist_observed_event(instance: &str, event: ObservedEvent) -> Result<bool> {
    if !super::native_surface_attached(instance) {
        return Ok(false);
    }
    let mut fold_navigation_into_click = false;
    let (action, receipt) = match event {
        ObservedEvent::Navigate { url, linked_click } => {
            validate_text(&url, MAX_OBSERVED_URL_BYTES, "URL")?;
            fold_navigation_into_click = linked_click;
            (
                BrowserUserAction::Navigate {
                    url: url.clone(),
                    new_tab: false,
                },
                BrowserInteractionReceipt {
                    action: "navigate".to_string(),
                    before_url: url.clone(),
                    after_url: url,
                    target: None,
                    sensitive: false,
                },
            )
        }
        ObservedEvent::Click { url, target } => {
            validate_text(&url, MAX_OBSERVED_URL_BYTES, "URL")?;
            validate_target(&target)?;
            (
                BrowserUserAction::Click { x: 0.0, y: 0.0 },
                BrowserInteractionReceipt {
                    action: "click".to_string(),
                    before_url: url.clone(),
                    after_url: url,
                    target: Some(target),
                    sensitive: false,
                },
            )
        }
        ObservedEvent::Type {
            url,
            target,
            mut value,
            sensitive,
        } => {
            validate_text(&url, MAX_OBSERVED_URL_BYTES, "URL")?;
            validate_target(&target)?;
            validate_text(&value, 64 * 1024, "field value")?;
            let sensitive = sensitive || super::semantic_target_is_sensitive(&target);
            if sensitive {
                // Defence in depth: the isolated-world script already sends
                // an empty value for sensitive fields. Never carry a value
                // farther if a future site/observer version marks it late.
                value.zeroize();
            }
            let text = if sensitive { String::new() } else { value };
            (
                BrowserUserAction::Type {
                    text,
                    clear: true,
                    sensitive,
                    parameter_name: None,
                    target_hint: Some(target.clone()),
                },
                BrowserInteractionReceipt {
                    action: "type".to_string(),
                    before_url: url.clone(),
                    after_url: url,
                    target: Some(target),
                    sensitive,
                },
            )
        }
        ObservedEvent::Select {
            url,
            target,
            option,
        } => {
            validate_text(&url, MAX_OBSERVED_URL_BYTES, "URL")?;
            validate_target(&target)?;
            validate_text(&option, 4 * 1024, "selected option")?;
            (
                BrowserUserAction::Select {
                    x: 0.0,
                    y: 0.0,
                    option,
                },
                BrowserInteractionReceipt {
                    action: "select".to_string(),
                    before_url: url.clone(),
                    after_url: url,
                    target: Some(target),
                    sensitive: false,
                },
            )
        }
    };
    crate::runtime::workflow_teaching::record_native_interaction(
        instance,
        action,
        receipt,
        fold_navigation_into_click,
    )
}

/// Attach one observer to a tab currently shown as a real native surface.
/// All durable writes happen on a bounded worker channel, never on Chrome's
/// CDP event thread; a slow fsync therefore cannot stall rendering or input.
pub(super) fn arm(tab: &Arc<Tab>, instance: &str) {
    if !super::native_surface_attached(instance) {
        return;
    }
    // A Chromium --app window exposes exactly one WebContents to the mounted
    // X11 child. Page JavaScript can otherwise use window.open() to create a
    // second root window/target that neither the user nor agent is looking at.
    // Install this in the page's main world before site scripts run. Unlike
    // the teaching binding below it exposes no Phoenix IPC or privileged data.
    if let Err(error) = tab.call_method(Page::AddScriptToEvaluateOnNewDocument {
        source: NATIVE_POPUP_CONTAINMENT_SCRIPT.to_string(),
        world_name: None,
        include_command_line_api: Some(false),
        run_immediately: Some(true),
    }) {
        tracing::warn!("native popup containment could not be installed: {error:#}");
    }
    let binding_name = format!("__phx_teach_{}", uuid::Uuid::new_v4().simple());
    let (sender, receiver) = sync_channel::<String>(128);
    let callback = Arc::new(move |payload: serde_json::Value| {
        let Some(payload) = payload.as_str() else {
            return;
        };
        if payload.len() > MAX_OBSERVED_EVENT_BYTES {
            return;
        }
        match sender.try_send(payload.to_string()) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            Err(TrySendError::Full(_)) => {
                // Never block Chrome's event loop. A human cannot produce 128
                // semantic actions before this local worker drains; overflow
                // means a hostile/broken page and is safest to drop.
            }
        }
    });
    if let Err(error) = tab
        .enable_runtime()
        .and_then(|_| tab.expose_raw_function_in_world(&binding_name, OBSERVER_WORLD, callback))
    {
        tracing::warn!("native teaching observer binding failed: {error:#}");
        return;
    }

    let binding_literal = serde_json::to_string(&binding_name)
        .expect("native teaching binding name is JSON-safe UUID text");
    let script = OBSERVER_SCRIPT.replace("__PHOENIX_BINDING__", &binding_literal);
    if let Err(error) = tab.call_method(Page::AddScriptToEvaluateOnNewDocument {
        source: script,
        world_name: Some(OBSERVER_WORLD.to_string()),
        include_command_line_api: Some(false),
        run_immediately: Some(true),
    }) {
        let _ = tab.remove_function(&binding_name);
        tracing::warn!("native teaching observer script failed: {error:#}");
        return;
    }

    let instance = instance.to_string();
    if let Err(error) = std::thread::Builder::new()
        .name(format!(
            "phoenix-teach-{}",
            if instance.is_empty() {
                "main"
            } else {
                &instance
            }
        ))
        .spawn(move || {
            while let Ok(payload) = receiver.recv() {
                let event = match serde_json::from_str::<ObservedEvent>(&payload) {
                    Ok(event) => event,
                    Err(error) => {
                        tracing::warn!("native teaching observer rejected an event: {error}");
                        continue;
                    }
                };
                match persist_observed_event(&instance, event) {
                    Ok(true) | Ok(false) => {}
                    Err(error) => {
                        tracing::warn!("native teaching step was not persisted: {error:#}");
                    }
                }
            }
        })
    {
        tracing::warn!("native teaching observer worker could not start: {error}");
    }
}

/// Native-only main-world policy. It collapses programmatic popup requests
/// into the already-mounted top-level page. A small inert Window-like handle
/// supports the common `open('', ...); popup.location = oauthUrl` pattern
/// without ever creating a hidden CDP target or a second X11 root window.
/// Only HTTP(S) destinations are allowed; `javascript:` and other active URL
/// schemes are refused.
const NATIVE_POPUP_CONTAINMENT_SCRIPT: &str = r###"
(() => {
  if (globalThis.__phoenixNativePopupPolicyInstalled) return;
  Object.defineProperty(globalThis, "__phoenixNativePopupPolicyInstalled", {
    value: true,
    configurable: false,
    enumerable: false,
    writable: false
  });

  const route = (raw) => {
    const value = String(raw == null ? "" : raw).trim();
    if (!value || value === "about:blank") return false;
    let destination;
    try { destination = new URL(value, location.href); } catch (_) { return false; }
    if (destination.protocol !== "http:" && destination.protocol !== "https:") return false;
    try {
      top.location.assign(destination.href);
      return true;
    } catch (_) {
      try { location.assign(destination.href); return true; } catch (_) { return false; }
    }
  };

  const makeHandle = () => {
    let closed = false;
    const locationFacade = Object.freeze({
      assign: (url) => route(url),
      replace: (url) => route(url),
      get href() { return String(top.location.href || ""); },
      set href(url) { route(url); },
      toString: () => String(top.location.href || "")
    });
    const handle = {
      close: () => { closed = true; },
      focus: () => {},
      blur: () => {},
      postMessage: (message, targetOrigin) => top.postMessage(message, targetOrigin || "*")
    };
    Object.defineProperties(handle, {
      closed: {get: () => closed},
      location: {get: () => locationFacade, set: (url) => { route(url); }},
      opener: {get: () => top},
      self: {get: () => handle},
      window: {get: () => handle}
    });
    return handle;
  };

  const containedOpen = function(url) {
    const handle = makeHandle();
    route(url);
    return handle;
  };
  try {
    Object.defineProperty(globalThis, "open", {
      value: containedOpen,
      configurable: false,
      enumerable: true,
      writable: false
    });
  } catch (_) {
    try { globalThis.open = containedOpen; } catch (_) {}
  }

  // Cover DOM-created popups too. Programmatic anchor.click() is untrusted,
  // so this containment policy intentionally does not use isTrusted (the
  // teaching observer still does). Modified/middle clicks are collapsed into
  // the mounted tab; ordinary target=_blank clicks/forms are rewritten before
  // their default action creates a second root window.
  const containAnchor = (event) => {
    const element = event.target && event.target.closest
      ? event.target.closest("a[href]")
      : null;
    if (!element) return;
    const modified = !!(event.ctrlKey || event.metaKey || event.shiftKey || event.altKey);
    const middle = event.type === "auxclick" && event.button === 1;
    const blank = String(element.target || "").toLowerCase() === "_blank";
    if (!modified && !middle && !blank) return;
    try { if (blank) element.target = "_self"; } catch (_) {}
    if ((modified || middle) && !element.hasAttribute("download")) {
      event.preventDefault();
      route(element.href);
    }
  };
  document.addEventListener("click", containAnchor, true);
  document.addEventListener("auxclick", containAnchor, true);
  document.addEventListener("submit", (event) => {
    const form = event.target;
    try {
      if (form && String(form.target || "").toLowerCase() === "_blank") {
        form.target = "_self";
      }
    } catch (_) {}
  }, true);

  // HTMLFormElement.submit() bypasses the submit event, so wrap that one
  // primitive as well. requestSubmit() already travels through the listener.
  try {
    const nativeSubmit = HTMLFormElement.prototype.submit;
    Object.defineProperty(HTMLFormElement.prototype, "submit", {
      value: function(...args) {
        try {
          if (String(this.target || "").toLowerCase() === "_blank") this.target = "_self";
        } catch (_) {}
        return Reflect.apply(nativeSubmit, this, args);
      },
      configurable: false,
      writable: false
    });
  } catch (_) {}
})();
"###;

/// Runs in a CDP isolated world. The website's scripts cannot see the random
/// binding or mutate this closure, while DOM events and element state remain
/// visible like a browser extension content script.
const OBSERVER_SCRIPT: &str = r###"
(() => {
  if (globalThis.__phoenixTeachingObserverInstalled) return;
  Object.defineProperty(globalThis, "__phoenixTeachingObserverInstalled", {value: true});
  const emitBinding = globalThis[__PHOENIX_BINDING__];
  if (typeof emitBinding !== "function") return;
  const clean = (value, max = 240) => String(value || "").replace(/\s+/g, " ").trim().slice(0, max);
  const cssEscape = (value) => globalThis.CSS && CSS.escape
    ? CSS.escape(String(value))
    : String(value).replace(/[^a-zA-Z0-9_-]/g, (char) => "\\" + char);
  const sensitive = (element) => {
    const signal = [
      element.type,
      element.autocomplete || element.getAttribute("autocomplete"),
      element.name,
      element.id,
      element.getAttribute("aria-label"),
      element.getAttribute("data-testid")
    ].join(" ").toLowerCase();
    return element.hasAttribute("data-phx-vault-secret") || element.type === "password" || /(password|passwd|one-time-code|one time code|verification code|security code|otp|totp|2fa|mfa|secret|api key|token|cc-number|ccnumber|cardnumber|card-number|credit card|cc-csc|card number|cvv|cvc)/.test(signal);
  };
  const selectorFor = (element) => {
    if (element.hasAttribute("data-phx-vault-secret")) return "[data-phx-vault-secret]";
    if (element.id) return "#" + cssEscape(element.id);
    const testId = element.getAttribute("data-testid");
    if (testId) return `[data-testid="${String(testId).replace(/"/g, '\\"')}"]`;
    const aria = element.getAttribute("aria-label");
    if (aria) return `${element.tagName.toLowerCase()}[aria-label="${String(aria).replace(/"/g, '\\"')}"]`;
    if (element.name) return `${element.tagName.toLowerCase()}[name="${String(element.name).replace(/"/g, '\\"')}"]`;
    const parts = [];
    let node = element;
    while (node && node.nodeType === 1 && parts.length < 6) {
      let part = node.tagName.toLowerCase();
      let index = 1;
      let sibling = node;
      while ((sibling = sibling.previousElementSibling)) {
        if (sibling.tagName === node.tagName) index += 1;
      }
      part += `:nth-of-type(${index})`;
      parts.unshift(part);
      node = node.parentElement;
    }
    return parts.join(">");
  };
  const targetFor = (element) => {
    if (!element || element.nodeType !== 1) return null;
    const interactive = element.closest && element.closest("input,textarea,select,button,a,[role],[contenteditable=true]");
    if (interactive) element = interactive;
    let label = element.getAttribute("aria-label") || "";
    if (!label && element.labels && element.labels.length) {
      label = element.labels[0].innerText || element.labels[0].textContent || "";
    }
    const tag = element.tagName.toLowerCase();
    const field = tag === "input" || tag === "textarea" || tag === "select" || element.isContentEditable;
    return {
      tag,
      role: element.getAttribute("role") || "",
      input_type: element.type || "",
      autocomplete: element.autocomplete || element.getAttribute("autocomplete") || "",
      name: element.name || "",
      element_id: element.id || "",
      label: clean(label),
      text: field ? "" : clean(element.innerText || element.textContent || ""),
      selector: selectorFor(element)
    };
  };
  const eventElement = (event) => {
    const path = typeof event.composedPath === "function" ? event.composedPath() : [];
    return path.find((node) => node && node.nodeType === 1) || event.target;
  };
  const emit = (event) => {
    try { emitBinding(JSON.stringify(event)); } catch (_) {}
  };

  let lastUrl = "";
  let lastTrustedClickAt = -Infinity;
  const observeNavigation = () => {
    const url = String(location.href || "");
    if (url !== lastUrl && /^https?:\/\//i.test(url)) {
      lastUrl = url;
      const linked_click = performance.now() - lastTrustedClickAt < 2000;
      lastTrustedClickAt = -Infinity;
      emit({kind: "navigate", url, linked_click});
    }
  };
  observeNavigation();
  setInterval(observeNavigation, 350);
  addEventListener("popstate", observeNavigation, true);
  addEventListener("hashchange", observeNavigation, true);

  const emittedValues = new WeakMap();
  const editable = (element) => element && element.closest
    ? element.closest("input,textarea,[contenteditable=true]")
    : null;
  const fieldValue = (element) => element.isContentEditable
    ? String(element.innerText || element.textContent || "")
    : String(element.value || "");
  const flushField = (element) => {
    element = editable(element);
    if (!element) return;
    const isSensitive = sensitive(element);
    const value = isSensitive ? "" : fieldValue(element).slice(0, 65536);
    const signature = isSensitive ? "__sensitive__" : value;
    if (emittedValues.get(element) === signature) return;
    const target = targetFor(element);
    if (!target) return;
    emittedValues.set(element, signature);
    emit({kind: "type", url: String(location.href || ""), target, value, sensitive: isSensitive});
  };
  document.addEventListener("focusin", (event) => {
    if (!event.isTrusted) return;
    const element = editable(eventElement(event));
    if (element) emittedValues.delete(element);
  }, true);
  document.addEventListener("blur", (event) => {
    if (event.isTrusted) flushField(eventElement(event));
  }, true);
  document.addEventListener("keydown", (event) => {
    if (event.isTrusted && event.key === "Enter") flushField(eventElement(event));
  }, true);
  document.addEventListener("change", (event) => {
    if (!event.isTrusted) return;
    const element = eventElement(event);
    const select = element && element.closest ? element.closest("select") : null;
    if (select) {
      const target = targetFor(select);
      const selected = select.options && select.selectedIndex >= 0 ? select.options[select.selectedIndex] : null;
      if (target && selected) emit({kind: "select", url: String(location.href || ""), target, option: clean(selected.text || selected.value, 4096)});
      return;
    }
    flushField(element);
  }, true);
  document.addEventListener("submit", (event) => {
    if (!event.isTrusted) return;
    const form = eventElement(event);
    try {
      if (form && form.matches && form.matches('form[target="_blank"]')) {
        form.setAttribute("target", "_self");
      }
    } catch (_) {}
  }, true);
  document.addEventListener("click", (event) => {
    if (!event.isTrusted) return;
    lastTrustedClickAt = performance.now();
    const element = eventElement(event);
    // A mounted --app window has one visible WebContents. Keep ordinary
    // target=_blank links/forms in that surface instead of spawning a hidden
    // top-level Chromium window the user cannot see inside Phoenix.
    const anchor = element && element.closest ? element.closest('a[target="_blank"]') : null;
    const form = element && element.closest ? element.closest('form[target="_blank"]') : null;
    try { if (anchor) anchor.setAttribute("target", "_self"); } catch (_) {}
    try { if (form) form.setAttribute("target", "_self"); } catch (_) {}
    const target = targetFor(element);
    if (target) emit({kind: "click", url: String(location.href || ""), target});
  }, true);
})();
"###;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observer_never_reads_sensitive_field_values() {
        assert!(OBSERVER_SCRIPT.contains("const value = isSensitive ? \"\""));
        assert!(OBSERVER_SCRIPT.contains("element.type === \"password\""));
        assert!(!OBSERVER_SCRIPT.contains("addEventListener(\"input\""));
        assert!(OBSERVER_SCRIPT.contains("addEventListener(\"submit\""));
        assert!(OBSERVER_SCRIPT.contains("if (!event.isTrusted) return;"));
    }

    #[test]
    fn native_popup_policy_reuses_the_mounted_http_surface() {
        assert!(NATIVE_POPUP_CONTAINMENT_SCRIPT.contains("top.location.assign"));
        assert!(NATIVE_POPUP_CONTAINMENT_SCRIPT.contains("set href(url)"));
        assert!(NATIVE_POPUP_CONTAINMENT_SCRIPT.contains("new URL(value, location.href)"));
        assert!(NATIVE_POPUP_CONTAINMENT_SCRIPT.contains("destination.protocol !== \"http:\""));
        assert!(NATIVE_POPUP_CONTAINMENT_SCRIPT.contains("HTMLFormElement.prototype, \"submit\""));
        assert!(!NATIVE_POPUP_CONTAINMENT_SCRIPT.contains("window.open("));
    }

    #[test]
    fn sensitive_payload_is_replaced_by_a_parameter_before_persistence() {
        let event = ObservedEvent::Type {
            url: "https://example.com/login".to_string(),
            target: BrowserSemanticTarget {
                tag: "input".to_string(),
                input_type: "password".to_string(),
                selector: "#password".to_string(),
                ..BrowserSemanticTarget::default()
            },
            value: "must-not-survive".to_string(),
            sensitive: false,
        };
        let ObservedEvent::Type {
            target,
            mut value,
            sensitive,
            ..
        } = event
        else {
            unreachable!()
        };
        let sensitive = sensitive || super::super::semantic_target_is_sensitive(&target);
        if sensitive {
            value.zeroize();
        }
        assert!(sensitive);
        assert!(value.is_empty());
    }
}
