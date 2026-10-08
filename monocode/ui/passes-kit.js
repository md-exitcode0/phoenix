"use strict";

// Passes kit: shared field definitions, card formatting/brand detection, and
// form rendering/collection for the inline ask_for_pass popup and the
// Settings → Passes manager. Secrets only ever live in form inputs and are
// sent straight to the gateway's Vault command; nothing here persists them.
(() => {
  const esc = (value) => String(value ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

  const icons = {
    login: '<svg viewBox="0 0 20 20"><circle cx="7" cy="10" r="3.2"/><path d="M10.2 10H17M14.5 10v2.6M16.8 10v1.8"/></svg>',
    card: '<svg viewBox="0 0 20 20"><rect x="2.5" y="4.5" width="15" height="11" rx="2.2"/><path d="M2.5 8h15M5.5 12.2h3"/></svg>',
    api_key: '<svg viewBox="0 0 20 20"><path d="M7 6 3 10l4 4M13 6l4 4-4 4M11.2 4.5 8.8 15.5"/></svg>',
    token: '<svg viewBox="0 0 20 20"><path d="M10 2.7 16 5v4.2c0 4.1-2.1 6.7-6 8.1-3.9-1.4-6-4-6-8.1V5l6-2.3Z"/><path d="M7.6 10h4.8M10 7.6v4.8"/></svg>',
    verification_code: '<svg viewBox="0 0 20 20"><rect x="3" y="5" width="14" height="10" rx="2.2"/><path d="M6 10h.1M8.7 10h.1M11.3 10h.1M14 10h.1"/></svg>',
    secret: '<svg viewBox="0 0 20 20"><rect x="4" y="8.5" width="12" height="8.5" rx="2"/><path d="M6.8 8.5V6.3a3.2 3.2 0 0 1 6.4 0v2.2M10 12v2"/></svg>',
    lock: '<svg viewBox="0 0 20 20"><rect x="4" y="8.5" width="12" height="8.5" rx="2"/><path d="M6.8 8.5V6.3a3.2 3.2 0 0 1 6.4 0v2.2"/></svg>',
    unlock: '<svg viewBox="0 0 20 20"><rect x="4" y="8.5" width="12" height="8.5" rx="2"/><path d="M6.8 8.5V6.3a3.2 3.2 0 0 1 6.2-1.1"/></svg>',
    eye: '<svg viewBox="0 0 20 20"><path d="M2 10s3-5.5 8-5.5S18 10 18 10s-3 5.5-8 5.5S2 10 2 10Z"/><circle cx="10" cy="10" r="2.4"/></svg>',
    eyeOff: '<svg viewBox="0 0 20 20"><path d="M3 3l14 14M8.3 5A8.4 8.4 0 0 1 10 4.5c5 0 8 5.5 8 5.5a14.6 14.6 0 0 1-2.4 3.1M5.3 6.6C3.2 8.1 2 10 2 10s3 5.5 8 5.5c1.4 0 2.6-.4 3.7-1"/></svg>',
    copy: '<svg viewBox="0 0 20 20"><rect x="7" y="7" width="9.5" height="9.5" rx="2"/><path d="M13 7V5a1.5 1.5 0 0 0-1.5-1.5h-6A1.5 1.5 0 0 0 4 5v6A1.5 1.5 0 0 0 5.5 12.5H7"/></svg>',
    check: '<svg viewBox="0 0 20 20"><path d="m5 10.5 3.2 3.2L15.5 6"/></svg>',
  };

  // Field catalogue. `secret` fields are masked; `primary` is the pass's main
  // secret; others become sealed secondary fields or public metadata.
  const FIELD = {
    site: { label: "Website", placeholder: "accounts.google.com", type: "text", autocomplete: "url", role: "site" },
    service: { label: "Service", placeholder: "api.openai.com", type: "text", autocomplete: "off", role: "site" },
    username: { label: "Email or username", placeholder: "you@example.com", type: "text", autocomplete: "username", role: "username" },
    password: { label: "Password", placeholder: "Password", type: "password", autocomplete: "current-password", secret: true, role: "primary" },
    totp: { label: "2FA setup key (optional)", placeholder: "Base32 key or otpauth:// link", type: "password", autocomplete: "off", secret: true, role: "field", optional: true },
    number: { label: "Card number", placeholder: "1234 5678 9012 3456", type: "text", inputmode: "numeric", autocomplete: "cc-number", secret: true, role: "primary", card: "number" },
    name: { label: "Name on card", placeholder: "Full name", type: "text", autocomplete: "cc-name", role: "field" },
    expiry: { label: "Expiry", placeholder: "MM / YY", type: "text", inputmode: "numeric", autocomplete: "cc-exp", role: "field", card: "expiry" },
    cvc: { label: "CVC", placeholder: "123", type: "password", inputmode: "numeric", autocomplete: "cc-csc", secret: true, role: "field", card: "cvc" },
    billing_zip: { label: "Billing ZIP / postal code", placeholder: "T2P 1J9", type: "text", autocomplete: "postal-code", role: "field" },
    key: { label: "API key", placeholder: "Paste the key", type: "password", autocomplete: "off", secret: true, role: "primary" },
    base_url: { label: "Base URL (optional)", placeholder: "https://api.example.com/v1", type: "text", autocomplete: "off", role: "meta", optional: true },
    value: { label: "Value", placeholder: "Paste the secret", type: "password", autocomplete: "off", secret: true, role: "primary" },
    code: { label: "Verification code", placeholder: "123456", type: "text", inputmode: "numeric", autocomplete: "one-time-code", secret: true, role: "primary", code: true },
  };
  const KINDS = {
    login: { label: "Login", plural: "Logins", stored: "password", fields: ["site", "username", "password", "totp"], icon: "login" },
    card: { label: "Card", plural: "Cards", stored: "card", fields: ["number", "name", "expiry", "cvc", "billing_zip"], icon: "card" },
    api_key: { label: "API key", plural: "API keys", stored: "api_key", fields: ["service", "key", "base_url"], icon: "api_key" },
    token: { label: "Token", plural: "Tokens", stored: "token", fields: ["service", "value"], icon: "token" },
    verification_code: { label: "Code", plural: "Codes", stored: "verification_code", fields: ["code"], icon: "verification_code" },
    secret: { label: "Secret", plural: "Secrets", stored: "secret", fields: ["value"], icon: "secret" },
  };
  // Stored kind → request kind.
  const fromStored = (kind) => ({ password: "login", card: "card", api_key: "api_key", token: "token", verification_code: "verification_code" }[kind] || "secret");
  const PRIMARY = { login: "password", card: "number", api_key: "key", token: "value", verification_code: "code", secret: "value" };

  const BRANDS = { visa: "Visa", mastercard: "Mastercard", amex: "Amex", discover: "Discover", diners: "Diners", jcb: "JCB", unionpay: "UnionPay", card: "Card" };
  function brand(digits) {
    const d = String(digits || "").replace(/\D/g, ""), n = (k) => Number(d.slice(0, k)) || 0;
    if (/^4/.test(d)) return "visa";
    if ((n(2) >= 51 && n(2) <= 55) || (n(4) >= 2221 && n(4) <= 2720)) return "mastercard";
    if (n(2) === 34 || n(2) === 37) return "amex";
    if (n(4) === 6011 || n(2) === 65 || (n(3) >= 644 && n(3) <= 649)) return "discover";
    if ([36, 38, 39].includes(n(2)) || (n(3) >= 300 && n(3) <= 305)) return "diners";
    if (n(4) >= 3528 && n(4) <= 3589) return "jcb";
    if (n(2) === 62) return "unionpay";
    return "card";
  }
  function luhn(digits) {
    const d = String(digits || "").replace(/\D/g, "");
    if (d.length < 12 || d.length > 19) return false;
    let sum = 0;
    for (let i = 0; i < d.length; i += 1) { let v = Number(d[d.length - 1 - i]); if (i % 2) { v *= 2; if (v > 9) v -= 9; } sum += v; }
    return sum % 10 === 0;
  }
  function formatCard(value) {
    const d = String(value || "").replace(/\D/g, "").slice(0, 19);
    const groups = brand(d) === "amex" ? [4, 6, 5] : [4, 4, 4, 4, 3];
    const out = []; let i = 0;
    for (const size of groups) { if (i >= d.length) break; out.push(d.slice(i, i + size)); i += size; }
    return out.join(" ");
  }
  function formatExpiry(value, deleting) {
    let d = String(value || "").replace(/\D/g, "").slice(0, 4);
    if (d.length === 1 && Number(d) > 1) d = `0${d}`;
    if (d.length >= 3 || (d.length === 2 && !deleting)) return `${d.slice(0, 2)} / ${d.slice(2)}`.trim();
    return d;
  }
  function expiryValid(value) {
    const m = String(value || "").match(/^(\d{2})\s*\/\s*(\d{2})$/);
    if (!m) return false;
    const month = Number(m[1]), year = 2000 + Number(m[2]), now = new Date();
    return month >= 1 && month <= 12 && (year > now.getFullYear() || (year === now.getFullYear() && month >= now.getMonth() + 1));
  }
  function brandBadge(name) {
    const key = BRANDS[name] ? name : "card";
    return `<span class="pass-brand pass-brand-${key}" aria-label="${esc(BRANDS[key])}">${esc(BRANDS[key])}</span>`;
  }
  function publicMeta(pass) { try { return JSON.parse(pass?.metadata_json || "{}") || {}; } catch { return {}; } }
  function summary(pass) {
    const meta = publicMeta(pass), kind = fromStored(pass.kind);
    if (kind === "card") return `${BRANDS[meta.brand] || "Card"} •••• ${meta.last4 || "····"}`;
    if (kind === "login") return [pass.username, pass.site].filter(Boolean).join(" · ");
    const site = pass.site === "phoenix.local" ? (KINDS[kind]?.label || "Secret") : pass.site;
    if (meta.last4) return `${meta.service || site} · ••••${meta.last4}`;
    return meta.service || site;
  }

  // Render a set of fields. `values` prefills; `labels` overrides labels.
  function renderFields(kind, fields, { labels = {}, values = {}, lockSite = false, editing = false } = {}) {
    const names = (fields && fields.length ? fields : KINDS[kind]?.fields || ["value"]).filter((name) => FIELD[name]);
    const cardRow = [];
    const html = names.map((name) => {
      const def = FIELD[name], value = values[name] ?? "", label = labels[name] || def.label;
      const keep = editing && (def.secret || def.role === "field"), optional = def.optional || keep ? "" : "required";
      const attrs = `name="${name}" data-pass-field="${name}" type="${def.type === "password" ? "password" : "text"}" ${def.inputmode ? `inputmode="${def.inputmode}"` : ""} autocomplete="${def.autocomplete}" spellcheck="false" placeholder="${esc(keep ? "Unchanged" : def.placeholder)}" value="${esc(value)}" ${optional} ${lockSite && def.role === "site" && value ? "readonly" : ""}`;
      const toggle = def.type === "password" ? `<button type="button" class="pass-reveal-toggle" data-pass-toggle aria-label="Show ${esc(label)}" aria-pressed="false">${icons.eye}</button>` : "";
      const trailing = def.card === "number" ? `<span class="pass-brand-slot" data-pass-brand></span>` : "";
      const field = `<label class="pass-field pass-field-${name}${def.code ? " pass-field-code" : ""}"><span>${esc(label)}</span><span class="pass-input"><input ${attrs}>${trailing}${toggle}</span><small class="pass-field-error" data-pass-error="${name}"></small></label>`;
      if (["expiry", "cvc"].includes(name)) { cardRow.push(field); return ""; }
      return field;
    });
    // Expiry + CVC share a row, right after the number/name.
    if (cardRow.length) {
      const index = Math.max(names.indexOf("name"), names.indexOf("number"));
      html.splice(index + 1, 0, `<div class="pass-field-row">${cardRow.join("")}</div>`);
    }
    return html.join("");
  }

  // Live formatting/brand detection; call once per form.
  function bind(form) {
    if (!form || form.dataset.passBound) return;
    form.dataset.passBound = "1";
    form.addEventListener("input", (event) => {
      const input = event.target, name = input.dataset?.passField;
      if (!name) return;
      const deleting = event.inputType?.startsWith("delete");
      if (name === "number") {
        const caretDigits = input.value.slice(0, input.selectionStart || 0).replace(/\D/g, "").length;
        input.value = formatCard(input.value);
        let pos = 0, seen = 0; while (pos < input.value.length && seen < caretDigits) { if (/\d/.test(input.value[pos])) seen += 1; pos += 1; }
        try { input.setSelectionRange(pos, pos); } catch {}
        const slot = form.querySelector("[data-pass-brand]"), b = brand(input.value);
        if (slot) { slot.innerHTML = input.value.replace(/\D/g, "").length >= 1 && b !== "card" ? brandBadge(b) : ""; slot.classList.toggle("on", b !== "card"); }
        const cvc = form.querySelector('[data-pass-field="cvc"]'); if (cvc) cvc.maxLength = b === "amex" ? 4 : 3;
      } else if (name === "expiry") input.value = formatExpiry(input.value, deleting);
      else if (name === "cvc" || name === "code") input.value = input.value.replace(/\D/g, "").slice(0, name === "cvc" ? 4 : 10);
      clearError(form, name);
    });
    form.addEventListener("click", (event) => {
      const button = event.target.closest("[data-pass-toggle]");
      if (!button) return;
      event.preventDefault();
      const input = button.parentElement.querySelector("input"), show = input.type === "password";
      input.type = show ? "text" : "password";
      button.setAttribute("aria-pressed", String(show));
      button.innerHTML = show ? icons.eyeOff : icons.eye;
      input.focus({ preventScroll: true });
    });
    form.querySelector('[data-pass-field="number"]')?.dispatchEvent(new Event("input", { bubbles: true }));
  }
  function setError(form, name, message) {
    const slot = form.querySelector(`[data-pass-error="${name}"]`), input = form.querySelector(`[data-pass-field="${name}"]`);
    if (slot) slot.textContent = message;
    input?.closest(".pass-field")?.classList.add("invalid");
    input?.focus();
  }
  function clearError(form, name) {
    const slot = form.querySelector(`[data-pass-error="${name}"]`);
    if (slot) slot.textContent = "";
    form.querySelector(`[data-pass-field="${name}"]`)?.closest(".pass-field")?.classList.remove("invalid");
  }

  // Validate and turn a form into the Vault command shape. Returns null and
  // marks the first invalid field when something is wrong.
  function collect(form, kind, { editing = false } = {}) {
    const read = (name) => String(form.querySelector(`[data-pass-field="${name}"]`)?.value ?? "").trim();
    const has = (name) => Boolean(form.querySelector(`[data-pass-field="${name}"]`));
    const primaryName = PRIMARY[kind] || "value";
    let secret = read(primaryName);
    if (kind === "card") {
      const digits = secret.replace(/\D/g, "");
      if (digits || !editing) {
        if (!luhn(digits)) { setError(form, "number", "That card number doesn't look right."); return null; }
        secret = formatCard(digits);
      }
      if (has("expiry") && read("expiry") && !expiryValid(read("expiry"))) { setError(form, "expiry", "Use a future MM / YY."); return null; }
      if (has("cvc") && read("cvc") && !/^\d{3,4}$/.test(read("cvc"))) { setError(form, "cvc", "3 or 4 digits."); return null; }
    }
    if (!secret && !editing) { setError(form, primaryName, "Required."); return null; }
    const fields = {}, meta = {};
    for (const name of Object.keys(FIELD)) {
      if (!has(name) || name === primaryName) continue;
      const def = FIELD[name], value = read(name);
      if (!value) continue;
      if (def.role === "field") fields[name] = value;
      else if (def.role === "meta") meta[name] = value;
    }
    if (meta.base_url && !/^https?:\/\//i.test(meta.base_url)) { setError(form, "base_url", "Start with https://"); return null; }
    if (kind === "api_key" || kind === "token") { const service = read("service"); if (service) meta.service = service; }
    const site = read("site") || read("service") || "";
    return { kind, site, username: read("username") || null, secret, fields, metadata: meta, secretChanged: Boolean(secret) || Object.values(fields).length > 0 };
  }

  function wipe(form) { form?.querySelectorAll("input").forEach((input) => { input.value = ""; }); }

  function toast(message, error) { window.PhoenixUI?.toast?.(message, error); }
  // Copy, then clear the clipboard after 30 s if it still holds this value.
  async function copySecret(value, label = "Copied") {
    try {
      await navigator.clipboard.writeText(value);
      toast(`${label} — clipboard clears in 30s`);
      setTimeout(async () => { try { if ((await navigator.clipboard.readText()) === value) await navigator.clipboard.writeText(""); } catch {} }, 30000);
    } catch { toast("Could not copy to the clipboard.", true); }
  }

  window.PhoenixPasses = { esc, icons, FIELD, KINDS, PRIMARY, BRANDS, fromStored, brand, luhn, formatCard, formatExpiry, expiryValid, brandBadge, publicMeta, summary, renderFields, bind, collect, setError, wipe, copySecret };
})();
