/**
 * ZHJ 加密登录（零依赖，可直接拷贝到下一工程）
 *
 * 协议（与后台约定，无需改后端）：
 * - POST {baseUrl}auth/login
 * - Header: encrypt-key = 预计算 RSA 密文（固定 AES 密钥）
 * - Header: clientid
 * - Body: AES-256-ECB+PKCS7(JSON) 的 Base64 字符串
 *
 * —— 用法（uni-app / 微信小程序）——
 *   import { login, createAuth } from '@/utils/zhj-login.js'
 *
 *   // 方式1：直接调（使用下方 DEFAULTS）
 *   const res = await login({ username: '138...', password: 'xxx' })
 *   // res 为接口 JSON：{ code, data: { access_token, ... }, ... }
 *
 *   // 方式2：自定义环境
 *   const auth = createAuth({
 *     baseUrl: 'https://xxx/dev-api/',
 *     clientId: '你的clientId',
 *   })
 *   const res = await auth.login({ username, password })
 *
 * —— 仅拿加密结果（自建请求）——
 *   import { buildEncryptedLogin } from '@/utils/zhj-login.js'
 *   const { url, header, data } = buildEncryptedLogin({ username, password })
 *   uni.request({ url, method: 'POST', header, data })
 */

// ======================== 可按环境修改 ========================
const DEFAULTS = {
	baseUrl: 'https://zhjmessaastest.clean-pro.cn:1081/dev-api/',
	clientId: 'e5cd7e4891bf95d1d19206ce24a7b32e',
	tenantId: '000000',
	appType: 'WxPro',
	grantType: 'password',
	loginPath: 'auth/login',
	getInfoPath: 'basic/user/getInfo',
	logoutPath: 'auth/logout',
	noCaptcha: 'noCaptcha'
};

/** 固定 32 字节 AES 密钥（须与 ENCRYPT_KEY 配套） */
const FIXED_AES_KEY = 'ZhjCloudWxProAesKey32BytesFixed!';

/**
 * RSA_PKCS1(Base64(FIXED_AES_KEY))，公钥见原工程 config.publicKey
 * 若更换公钥或密钥，需重新离线计算后替换此常量
 */
export const ENCRYPT_KEY =
	'RmVzz74GPsxD49n3BOPWuWjWCSb5e//yuXeXPXLpLY8OeNZTvQG8CMDyakGytDV5wXWNeHcJIIbXkX1g4e1Gcw==';

// ======================== AES-256-ECB（仅加密） ========================
const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';

const SBOX = [
	0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76, 0xca, 0x82,
	0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0, 0xb7, 0xfd, 0x93, 0x26,
	0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15, 0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96,
	0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75, 0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0,
	0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84, 0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb,
	0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf, 0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f,
	0x50, 0x3c, 0x9f, 0xa8, 0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff,
	0xf3, 0xd2, 0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
	0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb, 0xe0, 0x32,
	0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79, 0xe7, 0xc8, 0x37, 0x6d,
	0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08, 0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6,
	0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a, 0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e,
	0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e, 0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e,
	0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf, 0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f,
	0xb0, 0x54, 0xbb, 0x16
];

const RCON = [0x00, 0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36];

function utf8ToBytes(str) {
	const out = [];
	for (let i = 0; i < str.length; i++) {
		let c = str.charCodeAt(i);
		if (c < 0x80) out.push(c);
		else if (c < 0x800) out.push(0xc0 | (c >> 6), 0x80 | (c & 0x3f));
		else if (c < 0xd800 || c >= 0xe000) {
			out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f));
		} else {
			i++;
			c = 0x10000 + (((c & 0x3ff) << 10) | (str.charCodeAt(i) & 0x3ff));
			out.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 0x3f), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f));
		}
	}
	return out;
}

function bytesToBase64(bytes) {
	let s = '';
	for (let i = 0; i < bytes.length; i += 3) {
		const a = bytes[i];
		const b = i + 1 < bytes.length ? bytes[i + 1] : 0;
		const c = i + 2 < bytes.length ? bytes[i + 2] : 0;
		s += B64[a >> 2];
		s += B64[((a & 3) << 4) | (b >> 4)];
		s += i + 1 < bytes.length ? B64[((b & 15) << 2) | (c >> 6)] : '=';
		s += i + 2 < bytes.length ? B64[c & 63] : '=';
	}
	return s;
}

function subWord(w) {
	return (
		(SBOX[(w >>> 24) & 0xff] << 24) |
		(SBOX[(w >>> 16) & 0xff] << 16) |
		(SBOX[(w >>> 8) & 0xff] << 8) |
		SBOX[w & 0xff]
	);
}

function rotWord(w) {
	return ((w << 8) | (w >>> 24)) >>> 0;
}

function expandKey(keyBytes) {
	const Nk = 8;
	const Nr = 14;
	const w = new Array(4 * (Nr + 1));
	for (let i = 0; i < Nk; i++) {
		w[i] =
			((keyBytes[4 * i] << 24) |
				(keyBytes[4 * i + 1] << 16) |
				(keyBytes[4 * i + 2] << 8) |
				keyBytes[4 * i + 3]) >>>
			0;
	}
	for (let i = Nk; i < 4 * (Nr + 1); i++) {
		let temp = w[i - 1];
		if (i % Nk === 0) temp = (subWord(rotWord(temp)) ^ (RCON[i / Nk] << 24)) >>> 0;
		else if (i % Nk === 4) temp = subWord(temp);
		w[i] = (w[i - Nk] ^ temp) >>> 0;
	}
	return w;
}

function addRoundKey(state, w, round) {
	for (let c = 0; c < 4; c++) {
		const rw = w[round * 4 + c];
		state[c] ^= (rw >>> 24) & 0xff;
		state[4 + c] ^= (rw >>> 16) & 0xff;
		state[8 + c] ^= (rw >>> 8) & 0xff;
		state[12 + c] ^= rw & 0xff;
	}
}

function subBytes(state) {
	for (let i = 0; i < 16; i++) state[i] = SBOX[state[i]];
}

function shiftRows(state) {
	let t = state[4];
	state[4] = state[5];
	state[5] = state[6];
	state[6] = state[7];
	state[7] = t;
	t = state[8];
	state[8] = state[10];
	state[10] = t;
	t = state[9];
	state[9] = state[11];
	state[11] = t;
	t = state[15];
	state[15] = state[14];
	state[14] = state[13];
	state[13] = state[12];
	state[12] = t;
}

function xtime(a) {
	return ((a << 1) ^ (a & 0x80 ? 0x1b : 0)) & 0xff;
}

function mixColumns(state) {
	for (let c = 0; c < 4; c++) {
		const a0 = state[c];
		const a1 = state[4 + c];
		const a2 = state[8 + c];
		const a3 = state[12 + c];
		const r = a0 ^ a1 ^ a2 ^ a3;
		state[c] ^= r ^ xtime(a0 ^ a1);
		state[4 + c] ^= r ^ xtime(a1 ^ a2);
		state[8 + c] ^= r ^ xtime(a2 ^ a3);
		state[12 + c] ^= r ^ xtime(a3 ^ a0);
	}
}

function encryptBlock(input, w) {
	const state = [
		input[0],
		input[4],
		input[8],
		input[12],
		input[1],
		input[5],
		input[9],
		input[13],
		input[2],
		input[6],
		input[10],
		input[14],
		input[3],
		input[7],
		input[11],
		input[15]
	];
	addRoundKey(state, w, 0);
	for (let round = 1; round < 14; round++) {
		subBytes(state);
		shiftRows(state);
		mixColumns(state);
		addRoundKey(state, w, round);
	}
	subBytes(state);
	shiftRows(state);
	addRoundKey(state, w, 14);
	return [
		state[0],
		state[4],
		state[8],
		state[12],
		state[1],
		state[5],
		state[9],
		state[13],
		state[2],
		state[6],
		state[10],
		state[14],
		state[3],
		state[7],
		state[11],
		state[15]
	];
}

let cachedRoundKeys = null;

function getRoundKeys() {
	if (!cachedRoundKeys) cachedRoundKeys = expandKey(utf8ToBytes(FIXED_AES_KEY));
	return cachedRoundKeys;
}

/** AES-256-ECB + PKCS7 → Base64（兼容原 CryptoJS 输出） */
export function encryptWithAes(message) {
	const plain = utf8ToBytes(String(message));
	const pad = 16 - (plain.length % 16);
	for (let i = 0; i < pad; i++) plain.push(pad);
	const w = getRoundKeys();
	const out = [];
	for (let i = 0; i < plain.length; i += 16) {
		out.push(...encryptBlock(plain.slice(i, i + 16), w));
	}
	return bytesToBase64(out);
}

// ======================== 登录业务 ========================

function joinUrl(baseUrl, path) {
	const b = String(baseUrl || '').replace(/\/?$/, '/');
	const p = String(path || '').replace(/^\//, '');
	return b + p;
}

function mergeConfig(overrides) {
	return { ...DEFAULTS, ...(overrides || {}) };
}

/**
 * 组装明文登录参数（与本工程 api.login 一致）
 */
export function buildLoginParams(data, config) {
	const cfg = mergeConfig(config);
	const params = {
		...data,
		clientId: (data && data.clientId) || cfg.clientId,
		grantType: (data && data.grantType) || cfg.grantType,
		tenantId: (data && data.tenantId) || cfg.tenantId,
		appType: (data && data.appType) || cfg.appType,
		noCaptcha: (data && data.noCaptcha) || cfg.noCaptcha
	};
	return params;
}

/**
 * 生成可直接 uni.request / fetch 的加密登录请求
 * @returns {{ url, method, header, data }}
 */
export function buildEncryptedLogin(data, config) {
	const cfg = mergeConfig(config);
	const params = buildLoginParams(data, cfg);
	return {
		url: joinUrl(cfg.baseUrl, cfg.loginPath),
		method: 'POST',
		header: {
			'content-type': 'application/json',
			clientid: cfg.clientId,
			'Accept-language': 'zh-CN,zh;q=0.9,en;q=0.8',
			'content-language': 'zh_CN',
			'x-system-code': 'agent',
			'encrypt-key': ENCRYPT_KEY
		},
		data: encryptWithAes(JSON.stringify(params))
	};
}

function defaultRequest(options) {
	if (typeof uni !== 'undefined' && typeof uni.request === 'function') {
		return new Promise((resolve, reject) => {
			uni.request({
				...options,
				success: (res) => resolve(res.data),
				fail: reject
			});
		});
	}
	if (typeof fetch === 'function') {
		const init = {
			method: options.method || 'POST',
			headers: options.header
		};
		if (options.data != null && String(options.method || 'POST').toUpperCase() !== 'GET') {
			init.body = typeof options.data === 'string' ? options.data : JSON.stringify(options.data);
		}
		return fetch(options.url, init).then((r) => r.json());
	}
	return Promise.reject(new Error('未找到 uni.request 或 fetch，请传入 request 实现'));
}

function sanitizeToken(accessToken) {
	return String(accessToken || '')
		.trim()
		.replace(/^Bearer\s+/i, '')
		.replace(/[\r\n\t]/g, '');
}

/** 后端约定：authorization = Bearer <access_token>（只设一个头） */
function authHeaders(accessToken, extra) {
	const raw = sanitizeToken(accessToken);
	const authorization = raw ? `Bearer ${raw}` : '';
	return {
		...(extra || {}),
		authorization
	};
}

export function isOkCode(code) {
	return Number(code) === 200;
}

/** 从登录响应取出 access_token */
export function pickAccessToken(res) {
	const data = res && res.data && typeof res.data === 'object' ? res.data : res || {};
	return sanitizeToken(data.access_token || data.accessToken || data.token || '');
}

/**
 * 执行加密登录
 * @param {object} data  { username, password, smsCode?, noCaptcha?, ... }
 * @param {object} [config]  { baseUrl, clientId, tenantId, appType, grantType, request }
 * @returns {Promise<object>} 接口响应 JSON
 */
export function login(data, config) {
	const cfg = mergeConfig(config);
	const req = buildEncryptedLogin(data, cfg);
	const request = cfg.request || defaultRequest;
	return request(req);
}

/**
 * 用 access_token 拉取当前用户信息；code===200 表示登录未过期
 * Header: authorization = Bearer <access_token>
 * @param {string} accessToken
 * @param {object} [config]
 */
export function getInfo(accessToken, config) {
	const cfg = mergeConfig(config);
	const request = cfg.request || defaultRequest;
	return request({
		url: joinUrl(cfg.baseUrl, cfg.getInfoPath),
		method: 'GET',
		header: authHeaders(accessToken, {
			clientid: cfg.clientId
		})
	});
}

/**
 * 登出
 * @param {string} accessToken
 * @param {object} [config]
 */
export function logout(accessToken, config) {
	const cfg = mergeConfig(config);
	const request = cfg.request || defaultRequest;
	return request({
		url: joinUrl(cfg.baseUrl, cfg.logoutPath),
		method: 'POST',
		header: authHeaders(accessToken, {
			clientid: cfg.clientId,
			'content-type': 'application/json'
		}),
		data: '{}'
	});
}

/** 从 getInfo 响应中解析 userName / nickName / 公司 / 部门 */
export function parseUserProfile(res) {
	const root = res && typeof res === 'object' ? res : {};
	const data = root.data && typeof root.data === 'object' ? root.data : root;
	const user = data.user && typeof data.user === 'object' ? data.user : data;
	const deptObj =
		(user.dept && typeof user.dept === 'object' && user.dept) ||
		(data.dept && typeof data.dept === 'object' && data.dept) ||
		null;
	const companyObj =
		(user.company && typeof user.company === 'object' && user.company) ||
		(data.company && typeof data.company === 'object' && data.company) ||
		null;
	const companyName =
		user.companyName ||
		user.company_name ||
		data.companyName ||
		data.company_name ||
		(companyObj && (companyObj.companyName || companyObj.name || companyObj.deptName)) ||
		user.belongCompanyName ||
		data.belongCompanyName ||
		'';
	const deptName =
		user.deptName ||
		user.dept_name ||
		data.deptName ||
		data.dept_name ||
		(deptObj && (deptObj.deptName || deptObj.name)) ||
		'';
	return {
		userId: String(
			user.userId ?? user.user_id ?? user.id ?? data.userId ?? data.user_id ?? ''
		).trim(),
		userName: user.userName || user.username || user.user_name || data.userName || data.username || '',
		nickName: user.nickName || user.nickname || user.nick_name || data.nickName || data.nickname || '',
		companyName: companyName || '',
		deptName: deptName || ''
	};
}

/**
 * 工厂：绑定环境配置，便于多环境复用
 */
export function createAuth(config) {
	const cfg = mergeConfig(config);
	return {
		config: cfg,
		ENCRYPT_KEY,
		encryptWithAes,
		buildLoginParams: (data) => buildLoginParams(data, cfg),
		buildEncryptedLogin: (data) => buildEncryptedLogin(data, cfg),
		login: (data) => login(data, cfg),
		getInfo: (accessToken) => getInfo(accessToken, cfg),
		logout: (accessToken) => logout(accessToken, cfg),
		parseUserProfile,
		pickAccessToken,
		isOkCode
	};
}

export default {
	ENCRYPT_KEY,
	encryptWithAes,
	buildLoginParams,
	buildEncryptedLogin,
	login,
	getInfo,
	logout,
	parseUserProfile,
	pickAccessToken,
	isOkCode,
	createAuth
};
