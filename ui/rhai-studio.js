/**
 * 设置 → 脚本 →「脚本工作台」
 * 多轮对话生成 Rhai，并在沙箱试跑。
 */
(function (global) {
  const invoke = () => global.__TAURI__?.core?.invoke;
  const listen = () => global.__TAURI__?.event?.listen;

  const $ = (id) => document.getElementById(id);

  /** 添加附件一律落入 downloads（runtime 对 downloads/files 互通） */
  const ADD_KIND = "downloads";

  let listKind = "attachments";
  let sessionId = crypto.randomUUID();
  let busy = false;
  let unlistenChat = null;
  let opened = false;

  function escapeHtml(s) {
    return String(s)
      .replaceAll("&", "&amp;")
      .replaceAll("<", "&lt;")
      .replaceAll(">", "&gt;");
  }

  function setStatus(text) {
    const el = $("rhaiStudioHealth");
    if (el) el.textContent = text || "";
  }

  async function refreshHealth() {
    try {
      const h = await invoke()("rhai_studio_health");
      const bits = ["脚本工作台就绪"];
      bits.push(h.hasApiKey ? `模型 ${h.model}` : "未配置方舟 Key");
      setStatus(bits.join(" · "));
    } catch (e) {
      setStatus("后端异常: " + e);
    }
  }

  function syncAddButton() {
    const btn = $("rhaiStudioBtnAddFiles");
    if (!btn) return;
    btn.classList.toggle("hidden", listKind === "templates");
  }

  async function loadFiles() {
    const ul = $("rhaiStudioFileList");
    if (!ul) return;
    syncAddButton();
    try {
      const items = await invoke()("rhai_studio_list_files", { kind: listKind });
      ul.innerHTML = "";
      (items || []).forEach((it) => {
        const fileKind = it.kind || (listKind === "templates" ? "templates" : "downloads");
        const li = document.createElement("li");
        li.className = "rhai-studio-file";
        li.innerHTML = `<span class="name" title="${escapeHtml(it.name)}">${escapeHtml(it.name)}</span><button type="button" class="secondary del">删除</button>`;
        li.querySelector(".name").onclick = () => preview(fileKind, it.name);
        const delBtn = li.querySelector(".del");
        if (fileKind === "templates") {
          delBtn.disabled = true;
          delBtn.title = "请在设置 → 模板中管理";
        } else {
          delBtn.onclick = async (e) => {
            e.stopPropagation();
            try {
              await invoke()("rhai_studio_delete_file", {
                kind: fileKind,
                name: it.name,
              });
              loadFiles();
            } catch (err) {
              setStatus(String(err));
            }
          };
        }
        ul.appendChild(li);
      });
      if (!items || !items.length) {
        ul.innerHTML = `<li class="rhai-studio-empty">（空）</li>`;
      }
    } catch (e) {
      ul.innerHTML = `<li class="rhai-studio-empty">${escapeHtml(String(e))}</li>`;
    }
  }

  async function preview(fileKind, name) {
    const box = $("rhaiStudioFilePreview");
    if (!box) return;
    try {
      const data = await invoke()("rhai_studio_preview_file", {
        kind: fileKind,
        name,
      });
      if (data.rows) {
        box.textContent = data.rows.map((r) => r.join(" | ")).join("\n");
      } else if (data.text) {
        box.textContent = data.text;
      } else {
        box.textContent = data.error || "无法预览";
      }
    } catch (e) {
      box.textContent = String(e);
    }
  }

  function scriptDisplayName() {
    const n = ($("rhaiStudioScriptName")?.value || "").trim();
    return n || "未命名脚本";
  }

  function invokePrefix(name) {
    return `脚本「${name}」 `;
  }

  function argsSuffixFromLine(line) {
    const raw = String(line || "");
    const m = raw.match(/^脚本「[^」]*」\s*/);
    return m ? raw.slice(m[0].length) : raw.replace(/^\s+/, "");
  }

  function syncInvokeLine(opts) {
    const keepCursor = !!(opts && opts.keepCursor);
    const el = $("rhaiStudioScriptArgs");
    if (!el) return;
    const suffix = argsSuffixFromLine(el.value);
    const next = invokePrefix(scriptDisplayName()) + suffix;
    if (el.value === next) return;
    const start = el.selectionStart;
    const end = el.selectionEnd;
    const oldLen = el.value.length;
    el.value = next;
    if (keepCursor && document.activeElement === el) {
      const delta = next.length - oldLen;
      const ns = Math.max(invokePrefix(scriptDisplayName()).length, (start || 0) + delta);
      const ne = Math.max(ns, (end || 0) + delta);
      try {
        el.setSelectionRange(ns, ne);
      } catch (_) {}
    }
  }

  function argsForRun() {
    return argsSuffixFromLine($("rhaiStudioScriptArgs")?.value || "").trim();
  }

  function setScriptMeta(name, code) {
    if (name != null && $("rhaiStudioScriptName")) {
      $("rhaiStudioScriptName").value = name;
    }
    if (code != null && $("rhaiStudioScriptCode")) {
      $("rhaiStudioScriptCode").value = code;
    }
    syncInvokeLine();
  }

  function clearArtifacts() {
    const box = $("rhaiStudioArtifacts");
    if (!box) return;
    box.innerHTML = "";
    box.classList.add("hidden");
  }

  function renderArtifacts(artifacts) {
    const box = $("rhaiStudioArtifacts");
    if (!box) return;
    box.innerHTML = "";
    const list = Array.isArray(artifacts) ? artifacts : [];
    if (!list.length) {
      box.classList.add("hidden");
      return;
    }
    box.classList.remove("hidden");
    const title = document.createElement("div");
    title.className = "rhai-studio-artifacts-title";
    title.textContent = "产物";
    box.appendChild(title);
    list.forEach((a) => {
      const row = document.createElement("div");
      row.className = "rhai-studio-artifact";
      const name = document.createElement("span");
      name.className = "name";
      name.title = a.path || a.name || "";
      name.textContent = a.name || a.path || "产物";
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "secondary";
      btn.textContent = "打开";
      btn.onclick = async () => {
        if (!a.path) {
          setStatus("产物路径为空");
          return;
        }
        try {
          await invoke()("open_path_externally_cmd", { path: a.path });
        } catch (e) {
          setStatus(String(e));
        }
      };
      row.appendChild(name);
      row.appendChild(btn);
      box.appendChild(row);
    });
  }

  function addBubble(role, text, cls) {
    const el = document.createElement("div");
    el.className = `rhai-studio-bubble ${cls || role}`;
    el.textContent = text;
    $("rhaiStudioMessages")?.appendChild(el);
    el.scrollIntoView({ block: "end" });
    return el;
  }

  function renderAskGroup(questions) {
    const list = Array.isArray(questions) ? questions : [];
    if (!list.length) return;
    const root = $("rhaiStudioMessages");
    if (!root) return;

    const wrap = document.createElement("div");
    wrap.className = "rhai-studio-ask-group";
    const title = document.createElement("div");
    title.className = "rhai-studio-ask-title";
    title.textContent =
      list.length > 1
        ? `请分别完成本轮 ${list.length} 个问题，再提交（可选选项或自定义）`
        : "请选择或自定义后提交";
    wrap.appendChild(title);

    const answers = list.map((q, idx) => ({
      id: q.id || `q${idx + 1}`,
      prompt: q.prompt || `问题 ${idx + 1}`,
      allowMultiple: !!(q.allow_multiple || q.allowMultiple),
      selected: [],
      custom: "",
    }));

    list.forEach((q, idx) => {
      const box = document.createElement("div");
      box.className = "rhai-studio-ask-box";
      const qEl = document.createElement("div");
      qEl.className = "q";
      qEl.textContent = `${idx + 1}. ${q.prompt || "请选择"}`;
      box.appendChild(qEl);
      const opts = document.createElement("div");
      opts.className = "opts";
      const multi = answers[idx].allowMultiple;
      (q.options || []).forEach((opt) => {
        const b = document.createElement("button");
        b.type = "button";
        b.className = "secondary opt";
        b.textContent = opt.label || opt.id;
        b.onclick = () => {
          if (busy) return;
          const id = opt.id || opt.label;
          const label = opt.label || opt.id;
          answers[idx].custom = "";
          if (customInput) {
            customInput.value = "";
            customInput.classList.remove("active");
          }
          if (multi) {
            const i = answers[idx].selected.findIndex((s) => s.id === id);
            if (i >= 0) answers[idx].selected.splice(i, 1);
            else answers[idx].selected.push({ id, label });
            b.classList.toggle("picked", i < 0);
          } else {
            answers[idx].selected = [{ id, label }];
            opts.querySelectorAll(".opt").forEach((el) => el.classList.remove("picked"));
            b.classList.add("picked");
          }
          refresh();
        };
        opts.appendChild(b);
      });
      box.appendChild(opts);

      const customWrap = document.createElement("div");
      customWrap.className = "rhai-studio-ask-custom";
      const customLabel = document.createElement("div");
      customLabel.className = "label";
      customLabel.textContent = "或自定义回答";
      const customInput = document.createElement("input");
      customInput.type = "text";
      customInput.placeholder = "输入自己的答案…";
      customInput.addEventListener("input", () => {
        if (busy) return;
        const v = customInput.value.trim();
        answers[idx].custom = v;
        if (v) {
          answers[idx].selected = [];
          opts.querySelectorAll(".opt").forEach((el) => el.classList.remove("picked"));
          customInput.classList.add("active");
        } else {
          customInput.classList.remove("active");
        }
        refresh();
      });
      customInput.addEventListener("keydown", (e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          if (!submit.disabled) submit.click();
        }
      });
      customWrap.appendChild(customLabel);
      customWrap.appendChild(customInput);
      box.appendChild(customWrap);
      wrap.appendChild(box);
    });

    const foot = document.createElement("div");
    foot.className = "rhai-studio-ask-foot";
    const hint = document.createElement("span");
    hint.className = "hint";
    const submit = document.createElement("button");
    submit.type = "button";
    submit.textContent = "提交本轮选择";
    submit.disabled = true;
    submit.onclick = () => {
      if (busy || submit.disabled) return;
      const lines = ["用户本轮选择："];
      answers.forEach((a, i) => {
        let text = "";
        if (a.custom.trim()) text = `自定义：${a.custom.trim()}`;
        else text = a.selected.map((s) => s.label).join("、");
        lines.push(`${i + 1}. [${a.id}] ${a.prompt} → ${text}`);
      });
      wrap.classList.add("done");
      submit.disabled = true;
      sendMessage(lines.join("\n"));
    };
    foot.appendChild(hint);
    foot.appendChild(submit);
    wrap.appendChild(foot);

    function answerDone(a) {
      return a.selected.length > 0 || !!a.custom.trim();
    }

    function refresh() {
      const done = answers.filter(answerDone).length;
      hint.textContent = `已答 ${done}/${answers.length}`;
      submit.disabled = done !== answers.length;
    }
    refresh();

    root.appendChild(wrap);
    wrap.scrollIntoView({ block: "end" });
  }

  let validateFixDepth = 0;

  async function validateScript(code, opts) {
    const askAiFix = !!(opts && opts.askAiFix);
    const raw = (code || "").trim();
    if (!raw) return { ok: false, errors: ["脚本为空"], warnings: [] };
    try {
      const res = await invoke()("rhai_studio_validate_script", { script: raw });
      const errors = res.errors || [];
      const warnings = res.warnings || [];
      if (res.ok) {
        validateFixDepth = 0;
        let msg = "✓ 脚本校验通过";
        if (warnings.length) msg += "\n注意：\n- " + warnings.join("\n- ");
        addBubble("assistant", msg, "assistant");
        setStatus("脚本校验通过");
      } else {
        const lines = ["✗ 脚本校验未通过："];
        errors.forEach((e) => lines.push("- " + e));
        if (warnings.length) {
          lines.push("注意：");
          warnings.forEach((w) => lines.push("- " + w));
        }
        addBubble("assistant", lines.join("\n"), "error");
        setStatus("脚本校验未通过");
        if (askAiFix && validateFixDepth < 2) {
          validateFixDepth += 1;
          const fixMsg =
            "脚本校验未通过，请按下列问题修改后重新输出完整 rhai 块（不要 ask）：\n" +
            errors.map((e) => "- " + e).join("\n");
          setTimeout(() => sendMessage(fixMsg), 0);
        }
      }
      return res;
    } catch (e) {
      addBubble("assistant", "校验失败: " + e, "error");
      return { ok: false, errors: [String(e)], warnings: [] };
    }
  }

  function stripProtocol(s) {
    return s
      .replace(/```asks[\s\S]*?```/g, "")
      .replace(/```ask[\s\S]*?```/g, "")
      .replace(/```rhai[^\n]*\n([\s\S]*?)```/g, "\n（脚本已填入右侧测试区）\n")
      .trim();
  }

  async function ensureChatListener(thinkEl, textEl, state) {
    if (unlistenChat) {
      unlistenChat();
      unlistenChat = null;
    }
    unlistenChat = await listen()("rhai-studio-event", (ev) => {
      const payload = ev.payload || {};
      if (payload.sessionId && payload.sessionId !== sessionId) return;
      const event = payload.event;
      const data = payload.data || {};
      if (event === "thinking") {
        state.thinking += data.delta || "";
        thinkEl.textContent = state.thinking;
      }
      if (event === "text") {
        state.body += data.delta || "";
        textEl.textContent = stripProtocol(state.body);
      }
      if (event === "asks") {
        const qs = Array.isArray(data) ? data : data.questions || [];
        renderAskGroup(qs);
      }
      if (event === "ask") renderAskGroup([data]);
      if (event === "script") {
        setScriptMeta(data.name || "", data.code || "");
        state.gotScript = true;
      }
      if (event === "error") {
        textEl.className = "rhai-studio-bubble error";
        textEl.textContent = data.message || "请求失败";
      }
    });
  }

  async function sendMessage(text) {
    const input = $("rhaiStudioInput");
    const raw = (text || input?.value || "").trim();
    if (!raw || busy) return;
    if (input && !text) input.value = "";
    addBubble("user", raw);
    busy = true;
    const sendBtn = $("rhaiStudioBtnSend");
    if (sendBtn) sendBtn.disabled = true;
    const thinkEl = addBubble("assistant", "思考中…", "thinking");
    const textEl = addBubble("assistant", "", "assistant");
    const state = { thinking: "", body: "", gotScript: false };
    let shouldValidate = false;

    try {
      await ensureChatListener(thinkEl, textEl, state);
      const sid = await invoke()("rhai_studio_chat_send", {
        sessionId,
        message: raw,
      });
      if (sid) sessionId = sid;
      if (!state.thinking.trim()) thinkEl.remove();
      if (!textEl.textContent.trim() && !state.body) textEl.remove();
      shouldValidate = !!state.gotScript;
    } catch (e) {
      textEl.className = "rhai-studio-bubble error";
      textEl.textContent = String(e);
      thinkEl.remove();
    } finally {
      busy = false;
      if (sendBtn) sendBtn.disabled = false;
    }

    if (shouldValidate) {
      const code = ($("rhaiStudioScriptCode")?.value || "").trim();
      if (code) await validateScript(code, { askAiFix: true });
    }
  }

  function wireOnce() {
    if (wireOnce.done) return;
    wireOnce.done = true;

    $("rhaiStudioKindTabs")?.addEventListener("click", (e) => {
      const btn = e.target.closest("button[data-kind]");
      if (!btn) return;
      listKind = btn.dataset.kind === "templates" ? "templates" : "attachments";
      [...($("rhaiStudioKindTabs")?.querySelectorAll("button") || [])].forEach((b) => {
        const on = b === btn;
        b.classList.toggle("on", on);
        b.classList.toggle("secondary", !on);
      });
      const preview = $("rhaiStudioFilePreview");
      if (preview) preview.textContent = "选择文件可预览表头";
      loadFiles();
    });

    $("rhaiStudioBtnAddFiles")?.addEventListener("click", async () => {
      try {
        await invoke()("rhai_studio_pick_and_add_files", { kind: ADD_KIND });
        await loadFiles();
      } catch (e) {
        setStatus(String(e));
      }
    });

    $("rhaiStudioComposer")?.addEventListener("submit", (e) => {
      e.preventDefault();
      sendMessage();
    });

    $("rhaiStudioInput")?.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        sendMessage();
      }
    });

    $("rhaiStudioBtnReset")?.addEventListener("click", async () => {
      try {
        await invoke()("rhai_studio_reset_session", { sessionId });
      } catch (_) {}
      sessionId = crypto.randomUUID();
      const msgs = $("rhaiStudioMessages");
      if (msgs) msgs.innerHTML = "";
    });

    $("rhaiStudioBtnOpenWs")?.addEventListener("click", async () => {
      try {
        await invoke()("rhai_studio_open_workspace");
      } catch (e) {
        setStatus(String(e));
      }
    });

    $("rhaiStudioBtnAbort")?.addEventListener("click", async () => {
      try {
        await invoke()("rhai_studio_chat_abort", { sessionId });
      } catch (_) {}
    });

    $("rhaiStudioScriptName")?.addEventListener("input", () => {
      syncInvokeLine({ keepCursor: true });
    });

    $("rhaiStudioScriptArgs")?.addEventListener("blur", () => {
      syncInvokeLine();
    });

    $("rhaiStudioScriptArgs")?.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        $("rhaiStudioBtnRun")?.click();
      }
    });

    $("rhaiStudioBtnCopy")?.addEventListener("click", async () => {
      const code = $("rhaiStudioScriptCode")?.value || "";
      try {
        await navigator.clipboard.writeText(code);
        setStatus("已复制脚本");
      } catch (e) {
        setStatus(String(e));
      }
    });

    $("rhaiStudioBtnFill")?.addEventListener("click", () => {
      const name = $("rhaiStudioScriptName")?.value || "";
      const code = $("rhaiStudioScriptCode")?.value || "";
      if (!code.trim()) {
        setStatus("右侧脚本为空，无法填入");
        return;
      }
      document.dispatchEvent(
        new CustomEvent("rhai-studio-fill-script", { detail: { name, code } })
      );
    });

    $("rhaiStudioBtnRun")?.addEventListener("click", async () => {
      const logs = $("rhaiStudioRunLogs");
      if (logs) logs.textContent = "运行中…";
      clearArtifacts();
      try {
        const data = await invoke()("rhai_studio_run_script", {
          script: $("rhaiStudioScriptCode")?.value || "",
          args: argsForRun(),
        });
        const lines = [];
        lines.push(data.ok ? "✓ 成功" : "✗ 失败");
        if (data.error) lines.push(data.error);
        if (data.message) lines.push(data.message);
        if (data.logs && data.logs.length) {
          lines.push("—— 日志 ——");
          lines.push(data.logs.join("\n"));
        }
        if (logs) logs.textContent = lines.join("\n");
        renderArtifacts(data.artifacts || []);
        loadFiles();
      } catch (e) {
        if (logs) logs.textContent = String(e);
        clearArtifacts();
      }
    });

    $("rhaiStudioBtnClose")?.addEventListener("click", () => close());
    $("rhaiStudioModal")?.addEventListener("click", (e) => {
      if (e.target === $("rhaiStudioModal")) close();
    });
  }

  async function open() {
    wireOnce();
    const modal = $("rhaiStudioModal");
    if (!modal) return;
    modal.classList.remove("hidden");
    opened = true;
    await refreshHealth();
    await loadFiles();
    syncInvokeLine();
    $("rhaiStudioInput")?.focus();
  }

  function close() {
    const modal = $("rhaiStudioModal");
    if (modal) modal.classList.add("hidden");
    opened = false;
    if (busy) {
      try {
        invoke()("rhai_studio_chat_abort", { sessionId });
      } catch (_) {}
    }
  }

  global.RhaiStudio = { open, close, isOpen: () => opened };
})(window);
