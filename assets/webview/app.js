/* OpenCode Spotlight transcript page.
 *
 * Receives JSON via window.__bridge.receive(...) from the Rust host and
 * posts JSON strings back through window.webkit.messageHandlers.bridge.
 * The page trusts no network: vendored marked + DOMPurify + highlight.js.
 */
"use strict";

(function () {
  const root = document.getElementById("transcript");
  const FLUSH_MS = 70;

  /** id -> record */
  const messages = new Map();
  const order = [];

  function esc(text) {
    const div = document.createElement("div");
    div.textContent = text;
    return div.innerHTML;
  }

  function el(tag, cls, text) {
    const node = document.createElement(tag);
    if (cls) node.className = cls;
    if (text != null) node.textContent = text;
    return node;
  }

  function nearBottom() {
    return root.scrollHeight - root.scrollTop - root.clientHeight < 90;
  }
  let follow = true;
  root.addEventListener("scroll", () => {
    follow = nearBottom();
  });
  function scrollTop() {
    if (follow) root.scrollTop = root.scrollHeight;
  }

  function markdown(text) {
    let html = null;
    try {
      html = marked.parse(text);
    } catch (e) {
      html = "<p>" + esc(text) + "</p>";
    }
    return DOMPurify.sanitize(html);
  }

  function renderMarkdown(container, text) {
    container.innerHTML = markdown(text);
    container.querySelectorAll("pre > code").forEach((code) => {
      if (!code.dataset.hl) {
        try {
          hljs.highlightElement(code);
        } catch (e) {
          /* highlighting is best-effort */
        }
        code.dataset.hl = "1";
      }
      const pre = code.parentElement;
      if (!pre.querySelector(".copy-btn")) {
        pre.classList.add("codeblock");
        const btn = document.createElement("button");
        btn.className = "copy-btn";
        btn.type = "button";
        btn.textContent = "copy";
        btn.addEventListener("click", () => {
          const payload = { t: "copy", text: code.textContent || "" };
          try {
            window.webkit.messageHandlers.bridge.postMessage(JSON.stringify(payload));
            btn.textContent = "copied";
          } catch (e) {
            btn.textContent = "error";
          }
          setTimeout(() => {
            btn.textContent = "copy";
          }, 1200);
        });
        pre.appendChild(btn);
      }
    });
  }

  /* --- message records ----------------------------------------------------- */

  function ensure(id, kind) {
    let record = messages.get(id);
    if (record) return record;
    record = {
      id,
      kind,
      el: null,
      text: "",
      reasoning: "",
      images: [],
      tools: new Map(),
      streaming: false,
      dirty: true,
    };
    messages.set(id, record);
    order.push(id);
    render(record);
    scrollTop();
    return record;
  }

  function render(record) {
    if (!record.el) {
      if (record.kind === "user") {
        const wrap = el("div", "msg user");
        const text = el("div", "user-text");
        const images = el("div", "images");
        wrap.append(text, images);
        record.el = wrap;
        record.parts = { text, images };
        root.appendChild(wrap);
      } else {
        const wrap = el("div", "msg assistant");
        const agentLine = el("div", "agent-line");
        const reasoning = document.createElement("details");
        reasoning.className = "reasoning";
        const summary = el("summary", null, "thinking");
        const reasoningBody = el("div", "reasoning-body");
        reasoning.append(summary, reasoningBody);
        const body = el("div", "md");
        const tools = el("div", "tools");
        wrap.append(reasoning, body, tools);
        record.el = wrap;
        record.parts = { agentLine, reasoning, reasoningBody, body, tools };
        root.appendChild(wrap);
      }
    }
    if (record.kind === "user") {
      if (record.parts.text.textContent !== record.text) {
        record.parts.text.textContent = record.text;
      }
      if (record.images.length !== record.parts.images.childElementCount) {
        record.parts.images.replaceChildren(
          ...record.images.map((uri) => {
            const img = document.createElement("img");
            img.src = uri;
            return img;
          })
        );
      }
    } else {
      record.parts.agentLine.textContent = record.meta || "";
      if (record.reasoning) {
        record.parts.reasoning.open = record.streaming;
        if (record.parts.reasoningBody.dataset.raw !== record.reasoning) {
          record.parts.reasoningBody.dataset.raw = record.reasoning;
          record.parts.reasoningBody.textContent = record.reasoning;
        }
      } else {
        record.parts.reasoning.style.display = "none";
      }
      renderMarkdown(record.parts.body, record.text);
      record.parts.body.classList.toggle("stream-caret", record.streaming);
      renderTools(record);
    }
  }

  function renderTools(record) {
    const wanted = Array.from(record.tools.values());
    while (record.parts.tools.childElementCount > wanted.length) {
      record.parts.tools.removeChild(record.parts.tools.lastChild);
    }
    wanted.forEach((tool, index) => {
      let node = record.parts.tools.children[index];
      if (!node) {
        node = el("div", "tool");
        node.append(el("span", "tool-state"), el("span", "tool-name"), el("span", "tool-detail"));
        record.parts.tools.appendChild(node);
      }
      const state = tool.state || "…";
      const glyph =
        state === "success" ? "✓" : state === "failed" ? "✗" : state === "running" || state === "streaming" || state === "called" ? "▶" : "○";
      node.dataset.state = state;
      const [stateNode, nameNode, detailNode] = node.children;
      stateNode.textContent = glyph;
      nameNode.textContent = tool.name || "tool";
      detailNode.textContent = tool.detail || "";
      detailNode.title = tool.detail || "";
    });
  }

  const dirty = new Set();
  let flushScheduled = false;

  function markDirty(record) {
    record.dirty = true;
    dirty.add(record);
    if (!flushScheduled) {
      flushScheduled = true;
      setTimeout(flush, FLUSH_MS);
    }
  }

  function flush() {
    flushScheduled = false;
    for (const record of dirty) {
      if (record.dirty) {
        record.dirty = false;
        render(record);
      }
    }
    dirty.clear();
    scrollTop();
  }

  function assistantFromToolLine(id) {
    return ensure(id, "assistant");
  }

  /* --- inbound bridge -------------------------------------------------------- */

  function receive(payload) {
    follow = nearBottom();
    switch (payload.t) {
      case "theme": {
        document.body.classList.toggle("dark", !!payload.dark);
        break;
      }
      case "reset": {
        messages.clear();
        order.length = 0;
        root.replaceChildren();
        break;
      }
      case "init": {
        root.replaceChildren();
        messages.clear();
        order.length = 0;
        for (const body of payload.messages || []) {
          if (body.kind === "user") {
            const record = ensure(body.id, "user");
            record.text = body.text;
            record.images = body.images || [];
            record.dirty = true;
            dirty.add(record);
          } else {
            const record = ensure(body.id, "assistant");
            record.text = body.text;
            record.reasoning = body.reasoning;
            record.tools = new Map((body.tools || []).map((tool) => [tool.id, tool]));
            record.dirty = true;
            dirty.add(record);
          }
        }
        flush();
        break;
      }
      case "user": {
        const record = ensure(payload.id, "user");
        record.text = payload.text;
        record.images = payload.images || [];
        markDirty(record);
        flush();
        break;
      }
      case "text-delta": {
        const record = ensure(payload.message_id, "assistant");
        record.text += payload.delta;
        record.streaming = true;
        markDirty(record);
        break;
      }
      case "reasoning-delta": {
        const record = ensure(payload.message_id, "assistant");
        record.reasoning += payload.delta;
        record.streaming = true;
        markDirty(record);
        break;
      }
      case "tool": {
        const record = assistantFromToolLine(payload.message_id);
        const existing = record.tools.get(payload.tool_id) || {};
        const state = payload.state || existing.state;
        record.tools.set(payload.tool_id, {
          id: payload.tool_id,
          name: payload.name !== undefined && payload.name !== null ? payload.name : existing.name,
          state,
          detail: payload.detail !== undefined && payload.detail !== null ? payload.detail : existing.detail,
        });
        markDirty(record);
        break;
      }
      case "content-full": {
        const record = ensure(payload.message_id, "assistant");
        record.text = payload.text;
        record.reasoning = payload.reasoning;
        record.tools = new Map((payload.tools || []).map((tool) => [tool.id, tool]));
        record.streaming = false;
        markDirty(record);
        flush();
        break;
      }
      case "note": {
        const line = el("div", "note", payload.text);
        root.appendChild(line);
        scrollTop();
        break;
      }
      default:
        break;
    }
  }

  function lastAssistant() {
    for (let i = order.length - 1; i >= 0; i -= 1) {
      const record = messages.get(order[i]);
      if (record && record.kind === "assistant") return record;
    }
    return null;
  }

  window.__bridge = { receive };
})();
