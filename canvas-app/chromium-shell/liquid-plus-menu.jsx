import React, { useState } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import { Liquid } from "liquid-gooey";

let setMenuOpen = null;

function FileOrFolderIcon() {
  return (
    <svg viewBox="0 0 20 20" aria-hidden="true">
      <path d="M3 6.2h5.1l1.8 2H17" />
      <path d="M3 6.2v8.6h14V8.2" />
    </svg>
  );
}

function ImageIcon() {
  return (
    <svg viewBox="0 0 20 20" aria-hidden="true">
      <rect x="3" y="3.5" width="14" height="13" rx="2" />
      <path d="m4.8 14 3.8-4 2.7 2.6 1.8-1.7 2.1 2.2" />
    </svg>
  );
}

export function PlusMenu() {
  const [open, setOpen] = useState(false);
  setMenuOpen = setOpen;

  return (
    <Liquid
      id="attachmentMenu"
      className={`liquid-plus-menu${open ? " open" : ""}`}
      blur={3}
      contrast={18}
      fill="#fff"
      shadow="0 5px 10px rgba(0,0,0,.13)"
      filterPadding={20}
    >
      <Liquid.Item
        className="liquid-plus-item liquid-plus-file-item"
        x={open ? -19 : 0}
        y={open ? -34 : 0}
        transition="bouncy"
      >
        <button
          id="attachPathButton"
          className="liquid-plus-action"
          type="button"
          aria-label="Attach a file or folder"
          title="File or folder"
          tabIndex={open ? 0 : -1}
          aria-hidden={!open}
        >
          <FileOrFolderIcon />
        </button>
      </Liquid.Item>

      <Liquid.Item
        className="liquid-plus-item liquid-plus-image-item"
        x={open ? 19 : 0}
        y={open ? -34 : 0}
        transition="bouncy"
        delay={40}
      >
        <button
          id="attachImageButton"
          className="liquid-plus-action"
          type="button"
          aria-label="Attach images"
          title="Image"
          tabIndex={open ? 0 : -1}
          aria-hidden={!open}
        >
          <ImageIcon />
        </button>
      </Liquid.Item>

      <Liquid.Item className="liquid-plus-item liquid-plus-trigger-item">
        <button
          id="attachButton"
          className="composer-tool square-tool liquid-plus-trigger"
          type="button"
          aria-label="Add an attachment"
          title="Add attachment"
          aria-expanded={open}
          aria-controls="attachPathButton attachImageButton"
        >
          {/* A stroked plus, not the "+" character. That glyph's ink sits on
              the font's math axis rather than the centre of its em box, so as
              text it never centred inside the round trigger. The SVG keeps the
              135deg rotate-to-close animation. */}
          <span className="liquid-plus-glyph" aria-hidden="true">
            <svg viewBox="0 0 20 20"><path d="M10 4.6v10.8M4.6 10h10.8" /></svg>
          </span>
        </button>
      </Liquid.Item>
    </Liquid>
  );
}

const mount = document.getElementById("attachmentMenuMount");
if (mount) {
  const root = createRoot(mount);
  flushSync(() => root.render(<PlusMenu />));
  globalThis.phoenixLiquidAttachmentMenu = {
    setOpen(nextOpen) {
      if (!setMenuOpen) return;
      flushSync(() => setMenuOpen(Boolean(nextOpen)));
    },
  };
}
