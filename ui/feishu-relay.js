/**
 * webclaw ↔ feishu-relay-server WebSocket 客户端
 * 协议见 apps/feishu-relay-server/README.md
 * 中继地址由 Rust 内置配置，通过 get_feishu_relay_config_cmd 注入
 */
(function (global) {
  class FeishuRelayClient {
    constructor() {
      this.ws = null;
      this.host = "";
      this.port = 0;
      this.basePath = "/webclaw";
      this.clientId = "";
      this.clientSecret = "";
      this.state = {
        connected: false,
        authed: false,
        bound: false,
        chatId: null,
        bindCode: null,
        bindHint: null,
        lastError: null,
      };
      this.onStateChange = null;
      this.onFeishuMessage = null;
      /** 单飞 RPC（register/auth/bind_code/send_result），与入站 feishu_message 分离 */
      this._rpc = null;
      /** 串行发送，避免多个 send_message 互相覆盖 _rpc */
      this._sendChain = Promise.resolve();
      this._reconnectTimer = null;
      this._reconnecting = false;
    }

    _normalizePath(path) {
      const raw = (path ?? this.basePath ?? "/webclaw").trim();
      if (!raw || raw === "/") return "";
      const withSlash = raw.startsWith("/") ? raw : `/${raw}`;
      return withSlash.replace(/\/+$/, "");
    }

    _endpointUrl(scheme, suffix) {
      const h = (this.host || "").trim();
      const p = Number(this.port) || 0;
      if (!h || !p) {
        throw new Error("飞书 relay 未配置");
      }
      const base = this._normalizePath(this.basePath);
      return `${scheme}://${h}:${p}${base}${suffix}`;
    }

    wsUrl() {
      return this._endpointUrl("wss", "/ws");
    }

    _setState(patch) {
      Object.assign(this.state, patch);
      if (typeof this.onStateChange === "function") {
        this.onStateChange({ ...this.state });
      }
    }

    configure({ host, port, basePath, clientId, clientSecret }) {
      if (host !== undefined) this.host = host;
      if (port !== undefined) this.port = port;
      if (basePath !== undefined) this.basePath = basePath;
      if (clientId !== undefined) this.clientId = clientId || "";
      if (clientSecret !== undefined) this.clientSecret = clientSecret || "";
    }

    _clearRpc() {
      this._rpc = null;
    }

    _waitRpc(expectedTypes, timeoutMs, sendFn) {
      if (this._rpc) {
        return Promise.reject(new Error("飞书 relay 忙，请稍后重试"));
      }
      return new Promise((resolve, reject) => {
        const types = new Set(expectedTypes);
        const timer = setTimeout(() => {
          if (this._rpc === entry) this._clearRpc();
          reject(new Error("请求超时"));
        }, timeoutMs);
        const finish = (fn) => (arg) => {
          clearTimeout(timer);
          if (this._rpc === entry) this._clearRpc();
          fn(arg);
        };
        const entry = {
          match(msg) {
            if (msg.type === "error") {
              finish(reject)(new Error(msg.message || "relay 错误"));
              return true;
            }
            if (!types.has(msg.type)) return false;
            finish(resolve)(msg);
            return true;
          },
        };
        this._rpc = entry;
        try {
          sendFn();
        } catch (e) {
          clearTimeout(timer);
          this._clearRpc();
          reject(e);
        }
      });
    }

    disconnect(resetBinding = false) {
      if (this._reconnectTimer) {
        clearTimeout(this._reconnectTimer);
        this._reconnectTimer = null;
      }
      this._reconnecting = false;
      this._clearRpc();
      if (this.ws) {
        this.ws.close();
        this.ws = null;
      }
      const patch = {
        connected: false,
        authed: false,
        bindCode: null,
        bindHint: null,
      };
      if (resetBinding) {
        patch.bound = false;
        patch.chatId = null;
      }
      this._setState(patch);
    }

    _scheduleReconnect() {
      if (this._reconnectTimer || this._reconnecting) return;
      if (!this.clientId || !this.clientSecret) return;
      this._reconnectTimer = setTimeout(() => {
        this._reconnectTimer = null;
        this._reconnecting = true;
        this.connect({ force: true })
          .catch((e) => {
            this._setState({ lastError: String(e.message || e) });
          })
          .finally(() => {
            this._reconnecting = false;
          });
      }, 1500);
    }

    _send(obj) {
      if (!this.ws || this.ws.readyState !== 1) {
        throw new Error("未连接飞书 relay");
      }
      this.ws.send(JSON.stringify(obj));
    }

    _waitOpen(timeoutMs = 8000) {
      return new Promise((resolve, reject) => {
        if (this.ws?.readyState === 1) {
          resolve();
          return;
        }
        const t = setTimeout(() => reject(new Error("连接飞书 relay 超时")), timeoutMs);
        this.ws.addEventListener(
          "open",
          () => {
            clearTimeout(t);
            resolve();
          },
          { once: true }
        );
        this.ws.addEventListener(
          "error",
          () => {
            clearTimeout(t);
            reject(new Error("无法连接飞书 relay"));
          },
          { once: true }
        );
      });
    }

    async checkHealth(timeoutMs = 6000) {
      const tauriInvoke = global.__TAURI__?.core?.invoke;
      try {
        if (typeof tauriInvoke === "function") {
          return await tauriInvoke("check_feishu_relay_health_cmd");
        }
        if (!this.host || !this.port) {
          throw new Error("飞书 relay 未配置");
        }
        const url = this._endpointUrl("http", "/health");
        const ctrl = new AbortController();
        const timer = setTimeout(() => ctrl.abort(), timeoutMs);
        try {
          const res = await fetch(url, { signal: ctrl.signal });
          if (!res.ok) throw new Error(`HTTP ${res.status}`);
          const data = await res.json();
          if (!data || data.ok !== true) throw new Error("服务未就绪");
          return data;
        } finally {
          clearTimeout(timer);
        }
      } catch (e) {
        const detail =
          e && e.name === "AbortError" ? "请求超时" : String(e.message || e);
        throw new Error(`飞书 relay 不可用：${detail}`);
      }
    }

    async connect(options = {}) {
      const force = options.force === true;
      await this.checkHealth();
      if (
        !force &&
        this.ws?.readyState === 1 &&
        this.state.connected &&
        this.state.authed
      ) {
        return;
      }
      if (
        !force &&
        this.ws?.readyState === 1 &&
        this.state.connected &&
        this.clientId &&
        this.clientSecret
      ) {
        try {
          await this.auth();
          return;
        } catch (_) {
          /* fall through to full reconnect */
        }
      }
      const keepBound = this.state.bound;
      const keepChatId = this.state.chatId;
      this.disconnect(false);
      this._setState({ lastError: null, bound: keepBound, chatId: keepChatId });
      this.ws = new WebSocket(this.wsUrl());
      this.ws.onmessage = (ev) => this._onMessage(ev.data);
      this.ws.onclose = () => {
        const wasBound = this.state.bound;
        const chatId = this.state.chatId;
        this._setState({
          connected: false,
          authed: false,
          bound: wasBound,
          chatId,
        });
        if (wasBound && this.clientId && this.clientSecret) {
          this._scheduleReconnect();
        }
      };
      await this._waitOpen();
      this._setState({ connected: true });
      if (this.clientId && this.clientSecret) {
        await this.auth();
      }
    }

    async register(displayName) {
      await this.connect();
      const msg = await this._waitRpc(
        ["register_ok"],
        10000,
        () => this._send({ type: "register", displayName: displayName || undefined })
      );
      this.clientId = msg.clientId;
      this.clientSecret = msg.clientSecret;
      this._setState({ authed: true, bound: false });
      return { clientId: msg.clientId, clientSecret: msg.clientSecret };
    }

    async auth() {
      if (!this.clientId || !this.clientSecret) {
        throw new Error("缺少 clientId / clientSecret，请先注册");
      }
      if (!this.ws || this.ws.readyState !== 1) {
        await this.connect();
      }
      const msg = await this._waitRpc(["auth_ok"], 10000, () =>
        this._send({
          type: "auth",
          clientId: this.clientId,
          clientSecret: this.clientSecret,
        })
      );
      this._setState({
        authed: true,
        bound: !!msg.bound,
        chatId: msg.chatId || null,
      });
      return msg;
    }

    async getBindCode() {
      if (!this.state.authed) await this.auth();
      const msg = await this._waitRpc(["bind_code"], 10000, () =>
        this._send({ type: "get_bind_code" })
      );
      this._setState({
        bindCode: msg.code,
        bindHint: msg.hint || `绑定 ${msg.code}`,
      });
      return msg;
    }

    async _sendTextOnce(text) {
      if (!this.state.bound) throw new Error("尚未绑定飞书会话");
      return this._waitRpc(["send_result"], 20000, () =>
        this._send({ type: "send_message", text })
      ).then((msg) => {
        if (!msg.ok) throw new Error(msg.error || "发送失败");
        return msg;
      });
    }

    sendText(text) {
      const job = this._sendChain.then(() => this._sendTextOnce(text));
      this._sendChain = job.catch(() => {});
      return job;
    }

    _dispatchInbound(msg) {
      switch (msg.type) {
        case "feishu_message":
          if (typeof this.onFeishuMessage === "function") {
            this.onFeishuMessage(msg);
          }
          return;
        case "bind_ok":
          this._setState({
            bound: true,
            chatId: msg.chatId,
            bindCode: null,
          });
          return;
        default:
          break;
      }
    }

    _dispatchBroadcast(msg) {
      switch (msg.type) {
        case "auth_ok":
          this._setState({
            authed: true,
            bound: !!msg.bound,
            chatId: msg.chatId || null,
          });
          break;
        case "bind_code":
          this._setState({ bindCode: msg.code, bindHint: msg.hint });
          break;
        case "error":
          this._setState({ lastError: msg.message });
          break;
        default:
          break;
      }
    }

    _onMessage(raw) {
      let msg;
      try {
        msg = JSON.parse(raw);
      } catch {
        return;
      }

      // 入站飞书消息优先，永不因出站 RPC 等待而丢弃
      if (msg.type === "feishu_message" || msg.type === "bind_ok") {
        this._dispatchInbound(msg);
      }

      if (this._rpc && this._rpc.match(msg)) {
        return;
      }

      this._dispatchBroadcast(msg);

      if (msg.type === "feishu_message" || msg.type === "bind_ok") {
        return;
      }
    }
  }

  global.FeishuRelayClient = FeishuRelayClient;
})(typeof window !== "undefined" ? window : globalThis);
