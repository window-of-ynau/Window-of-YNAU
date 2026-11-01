const messages = document.getElementById('messages');
const input = document.getElementById('input');
const sendBtn = document.getElementById('send');
const attachBtn = document.getElementById('attachBtn');
const fileInput = document.getElementById('fileInput');
const fileChip = document.getElementById('fileChip');
const fileChipName = document.getElementById('fileChipName');
const fileChipClose = document.getElementById('fileChipClose');
const newChatBtn = document.getElementById('newChatBtn');
const themeToggle = document.getElementById('themeToggle');
const menuToggle = document.getElementById('menuToggle');
const sidebar = document.getElementById('sidebar');
const sidebarOverlay = document.getElementById('sidebarOverlay');
const sidebarCollapse = document.getElementById('sidebarCollapse');
const memoryBtn = document.getElementById('memoryBtn');
const memoryModal = document.getElementById('memoryModal');
const memoryBackdrop = document.getElementById('memoryBackdrop');
const memoryClose = document.getElementById('memoryClose');
const memoryBody = document.getElementById('memoryBody');
const memoryRefresh = document.getElementById('memoryRefresh');
const memoryReset = document.getElementById('memoryReset');

const SUGGESTIONS = ['今天有什么课？', '怎么查成绩', '图书馆开放时间', '教务系统在哪里'];

// ---------- 主题 ----------
function applyTheme(theme) {
  document.documentElement.setAttribute('data-theme', theme);
  document.getElementById('iconSun').style.display = theme === 'dark' ? 'none' : '';
  document.getElementById('iconMoon').style.display = theme === 'dark' ? '' : 'none';
  localStorage.setItem('theme', theme);
  // 同步移动端状态栏 / 浏览器工具栏颜色
  const meta = document.querySelector('meta[name="theme-color"]');
  if (meta) meta.setAttribute('content', theme === 'dark' ? '#0f172a' : '#ffffff');
}
applyTheme(localStorage.getItem('theme') ||
  (window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'));
themeToggle.onclick = () => applyTheme(document.documentElement.getAttribute('data-theme') === 'dark' ? 'light' : 'dark');

// ---------- 工具函数 ----------
function escapeHtml(s) {
  return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

// ---------- Markdown 渲染 ----------
function inlineMd(s) {
  return s
    .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
    .replace(/(^|[^*])\*([^*\s][^*]*)\*/g, '$1<em>$2</em>')
    .replace(/`([^`]+)`/g, '<code>$1</code>')
    .replace(/\[([^\]]+)\]\((https?:[^)\s]+)\)/g, (m, text, url) =>
      `<a class="md-btn" href="${url}" target="_blank" rel="noopener">${text} ↗</a>`);
}

function renderMarkdown(src) {
  const lines = escapeHtml(src).split('\n');
  const out = [];
  let i = 0, codeBuf = null, table = null, listType = null;

  const flushTable = () => {
    if (!table) return;
    let t = '<table>';
    table.rows.forEach((r, idx) => {
      t += '<tr>' + r.map(c => `<${idx === 0 ? 'th' : 'td'}>${c}</${idx === 0 ? 'th' : 'td'}>`).join('') + '</tr>';
    });
    t += '</table>';
    out.push(`<div class="table-wrap">${t}</div>`); table = null;
  };
  const flushList = () => {
    if (!listType) return;
    out.push(`</${listType}>`); listType = null;
  };

  while (i < lines.length) {
    const raw = lines[i];
    const t = raw.trim();

    if (t.startsWith('```')) {
      flushTable(); flushList();
      if (codeBuf) { out.push(`<pre><code>${codeBuf.join('\n')}</code></pre>`); codeBuf = null; }
      else codeBuf = [];
      i++; continue;
    }
    if (codeBuf) { codeBuf.push(raw); i++; continue; }

    if (t.startsWith('|') && t.endsWith('|')) {
      flushList();
      if (t.replace(/[\|\-\s:]/g, '') === '') { i++; continue; }
      const cells = t.slice(1, -1).split('|').map(c => inlineMd(c.trim()));
      if (!table) table = { rows: [cells] };
      else table.rows.push(cells);
      i++; continue;
    }
    flushTable();

    const h = t.match(/^(#{1,4})\s+(.*)$/);
    if (h) { flushList(); out.push(`<h${h[1].length}>${inlineMd(h[2])}</h${h[1].length}>`); i++; continue; }

    const ul = t.match(/^[-*]\s+(.*)$/);
    const ol = t.match(/^\d+[.)]\s+(.*)$/);
    if (ul || ol) {
      const type = ul ? 'ul' : 'ol';
      const content = inlineMd((ul || ol)[1]);
      if (listType !== type) { flushList(); out.push(`<${type}>`); listType = type; }
      out.push(`<li>${content}</li>`);
      i++; continue;
    }
    flushList();

    if (t === '') { out.push(''); i++; continue; }
    out.push(`<p>${inlineMd(t)}</p>`);
    i++;
  }
  flushTable(); flushList();
  if (codeBuf) out.push(`<pre><code>${codeBuf.join('\n')}</code></pre>`);
  return out.join('');
}

// ---------- 消息渲染 ----------
/* 极简结构：不带头像、不带昵称和昵称时间行。
   区分角色全靠位置和颜色——提问右侧蓝色气泡，回答左侧纯文本。 */
function addMsg(role, text) {
  const wrap = document.createElement('div');
  wrap.className = 'msg ' + role;

  const body = document.createElement('div');
  body.className = 'msg-body';

  const bubble = document.createElement('div');
  bubble.className = 'bubble';
  body.appendChild(bubble);
  wrap.appendChild(body);
  messages.appendChild(wrap);

  if (role === 'ai') {
    if (hasMarkdown(text)) {
      bubble.innerHTML = renderMarkdown(text);
      addCopyButton(body, text);
    } else {
      typeWriter(bubble, text, () => addCopyButton(body, text));
    }
  } else {
    bubble.textContent = text;
  }
  scrollBottom();
}

function hasMarkdown(text) {
  return /^#{1,4}\s/m.test(text) || text.includes('|') || text.includes('```') ||
         text.includes('](') || text.includes('**') || text.includes('`') ||
         text.includes('- ') || /^\d+[.)]\s/m.test(text);
}

function addCopyButton(host, rawText) {
  const btn = document.createElement('button');
  btn.className = 'copy-btn';
  btn.title = '复制';
  btn.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="12" height="12" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>';
  btn.onclick = async () => {
    try { await navigator.clipboard.writeText(rawText); }
    catch (e) { /* 忽略剪贴板权限失败 */ }
    btn.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6L9 17l-5-5"/></svg>';
    setTimeout(() => {
      btn.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="12" height="12" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>';
    }, 1200);
  };
  host.appendChild(btn);
}

function typeWriter(el, text, done) {
  let i = 0;
  el.innerHTML = '';
  const step = () => {
    if (i >= text.length) {
      el.textContent = text;
      if (done) done();
      return;
    }
    i++;
    el.textContent = text.slice(0, i);
    el.insertAdjacentHTML('beforeend', '<span class="cursor">▍</span>');
    scrollBottom();
    setTimeout(step, 14);
  };
  step();
}

function showTyping() {
  const wrap = document.createElement('div');
  wrap.className = 'msg ai';
  wrap.id = 'typing';
  wrap.innerHTML = '<div class="msg-body"><div class="bubble typing" role="status">正在生成回复…</div></div>';
  messages.appendChild(wrap);
  scrollBottom();
}
function hideTyping() { const t = document.getElementById('typing'); if (t) t.remove(); }

function scrollBottom() { messages.scrollTop = messages.scrollHeight; }

// ---------- 欢迎与快捷提问 ----------
const chatArea = document.getElementById('chatArea');
const heroChips = document.getElementById('heroChips');
const hero = document.getElementById('hero');
const inputBar = document.getElementById('inputBar');

// 平移动画参数（尊重系统「减弱动态效果」设置；企业 UI 一律 ≤ 200ms）
const SLIDE_MS = 200;
const SLIDE_EASE = 'cubic-bezier(0.16, 1, 0.3, 1)';
const reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

function cancelInputBarAnim() {
  inputBar.getAnimations().forEach(a => a.cancel());
}

let heroExitAnim = null;
function resetHero() {
  if (heroExitAnim) { heroExitAnim.cancel(); heroExitAnim = null; }
  hero.style.cssText = '';
}

// 用 FLIP 让输入框从旧位置平滑滑到新位置
function slideInputBar(fromTop) {
  if (reduceMotion) return;
  const toTop = inputBar.getBoundingClientRect().top;
  const delta = fromTop - toTop;
  if (Math.abs(delta) < 1) return;
  inputBar.animate(
    [{ transform: `translateY(${delta}px)` }, { transform: 'translateY(0)' }],
    { duration: SLIDE_MS, easing: SLIDE_EASE }
  );
}

function renderWelcome(animate) {
  resetHero();
  cancelInputBarAnim();

  const wasChat = !chatArea.classList.contains('is-empty');
  const fromTop = wasChat ? inputBar.getBoundingClientRect().top : 0;

  messages.innerHTML = '';
  chatArea.classList.add('is-empty');
  heroChips.innerHTML = '';
  SUGGESTIONS.forEach(q => {
    const c = document.createElement('button');
    c.className = 'chip';
    c.textContent = q;
    c.onclick = () => send(q);
    heroChips.appendChild(c);
  });

  if (animate && wasChat) {
    slideInputBar(fromTop);
  }
}

// 开始对话：标语淡出、输入框平移到页面底部，消息区淡入
function enterChatMode() {
  if (!chatArea.classList.contains('is-empty')) return;
  resetHero();
  cancelInputBarAnim();

  const heroRect = hero.getBoundingClientRect();
  const areaRect = chatArea.getBoundingClientRect();
  const fromTop = inputBar.getBoundingClientRect().top;

  chatArea.classList.remove('is-empty');
  slideInputBar(fromTop);

  if (!reduceMotion) {
    // 标语留在原地淡出上移，避免瞬间消失
    if (heroRect.height > 0) {
      hero.style.display = 'block';
      hero.style.position = 'absolute';
      hero.style.left = '0';
      hero.style.right = '0';
      hero.style.top = Math.max(0, heroRect.top - areaRect.top) + 'px';
      hero.style.pointerEvents = 'none';
      hero.style.animation = 'none';
      const anim = hero.animate(
        [
          { opacity: 1, transform: 'translateY(0)' },
          { opacity: 0, transform: 'translateY(-12px)' }
        ],
        { duration: 200, easing: 'ease-out', fill: 'forwards' }
      );
      heroExitAnim = anim;
      anim.onfinish = () => {
        heroExitAnim = null;
        hero.style.cssText = '';
        anim.cancel();
      };
    }
    messages.animate(
      [{ opacity: 0 }, { opacity: 1 }],
      { duration: 200, easing: 'ease-out' }
    );
  }
  // 不再自动插入开场白：空状态里的 hero 标语已经说明了这是什么，
  // 第一屏只留用户自己问的第一句。
}

// ---------- 发送逻辑 ----------
let busy = false;
let activeRequest = null;

async function send(text) {
  text = (typeof text === 'string' ? text : input.value).trim();
  if (!text || busy) return;
  busy = true;

  enterChatMode();
  addMsg('user', text);
  input.value = '';
  autoResize();
  sendBtn.disabled = true;
  showTyping();

  const controller = new AbortController();
  activeRequest = controller;
  try {
    const res = await fetch('/api/chat', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ message: text }),
      signal: controller.signal
    });
    const data = await res.json();
    hideTyping();
    addMsg('ai', data.reply || '（无回复）');
  } catch (e) {
    hideTyping();
    if (e.name !== 'AbortError') addMsg('ai', '出错了：' + e.message);
  } finally {
    // 已被「新对话」接管时不要再改状态，避免打断新一轮对话
    if (activeRequest !== controller) return;
    activeRequest = null;
    busy = false;
    sendBtn.disabled = false;
    input.focus();
  }
}

// ---------- 附件上传（图片 / 文档共用同一个回形针入口） ----------
attachBtn.onclick = () => fileInput.click();

function isImageFile(file) {
  if ((file.type || '').startsWith('image/')) return true;
  return /\.(png|jpe?g|gif|webp|bmp|heic|heif|avif|svg)$/i.test(file.name || '');
}

async function uploadImage(file) {
  enterChatMode();
  addMsg('user', '📷 [图片] ' + file.name);
  busy = true; sendBtn.disabled = true; showTyping();

  const fd = new FormData();
  fd.append('file', file);
  const controller = new AbortController();
  activeRequest = controller;
  try {
    const res = await fetch('/api/vision', { method: 'POST', body: fd, signal: controller.signal });
    const data = await res.json();
    hideTyping();
    addMsg('ai', data.reply || '（无回复）');
  } catch (e) {
    hideTyping();
    if (e.name !== 'AbortError') addMsg('ai', '识图失败：' + e.message);
  } finally {
    if (activeRequest !== controller) return;
    activeRequest = null;
    busy = false; sendBtn.disabled = false; fileInput.value = '';
  }
}

// 文档上传（qwen-long 文档问答）
async function uploadDoc(file) {
  enterChatMode();
  addMsg('user', '📄 [文档] ' + file.name);
  busy = true; sendBtn.disabled = true; showTyping();

  const fd = new FormData();
  fd.append('file', file);
  const controller = new AbortController();
  activeRequest = controller;
  try {
    const res = await fetch('/api/upload_file', { method: 'POST', body: fd, signal: controller.signal });
    const data = await res.json();
    hideTyping();
    if (data.ok) {
      setActiveFile({ file_id: data.file_id, name: data.name || file.name });
      addMsg('ai', `📄 文档《${data.name || file.name}》已上传，接下来可以直接提问关于它的内容。`);
    } else {
      addMsg('ai', '文档上传失败：' + (data.error || '未知错误'));
    }
  } catch (e) {
    hideTyping();
    if (e.name !== 'AbortError') addMsg('ai', '文档上传失败：' + e.message);
  } finally {
    if (activeRequest !== controller) return;
    activeRequest = null;
    busy = false; sendBtn.disabled = false; fileInput.value = '';
  }
}

fileInput.onchange = () => {
  const file = fileInput.files[0];
  if (!file) return;
  if (isImageFile(file)) uploadImage(file);
  else uploadDoc(file);
};

function setActiveFile(state) {
  if (state && state.file_id) {
    fileChipName.textContent = state.name || '已上传文档';
    fileChip.style.display = 'flex';
  } else {
    fileChip.style.display = 'none';
  }
}

async function loadFileState() {
  try {
    const res = await fetch('/api/file_state');
    setActiveFile(await res.json());
  } catch (e) { /* 忽略 */ }
}

fileChipClose.onclick = async () => {
  try { await fetch('/api/upload_file/clear', { method: 'POST' }); } catch (e) { /* 忽略 */ }
  setActiveFile(null);
  addMsg('ai', '已移除当前文档，恢复普通问答模式。');
};

// ---------- 输入框自适应高度 ----------
function autoResize() {
  input.style.height = 'auto';
  input.style.height = Math.min(input.scrollHeight, 140) + 'px';
}
input.addEventListener('input', autoResize);

// ---------- 事件绑定 ----------
sendBtn.onclick = () => send();
input.addEventListener('keydown', e => {
  if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) { e.preventDefault(); send(); }
});

// ---------- 新对话：清空后端记忆 + 前端气泡 ----------
async function startNewChat() {
  if (newChatBtn.disabled) return;
  newChatBtn.disabled = true;

  // 中断仍在生成的回答，避免它稍后把气泡和记忆又写回来
  if (activeRequest) {
    activeRequest.abort();
    activeRequest = null;
  }
  hideTyping();
  busy = false;
  sendBtn.disabled = false;
  input.value = '';
  fileInput.value = '';
  setActiveFile(null); // 新对话不再沿用上一轮挂载的文档
  autoResize();
  renderWelcome(true);

  try {
    const [memRes] = await Promise.all([
      fetch('/api/clear', { method: 'POST' }),
      // 后端文档状态一并复位；失败不影响对话记忆的清理
      fetch('/api/upload_file/clear', { method: 'POST' }).catch(() => null)
    ]);
    if (!memRes.ok) throw new Error('HTTP ' + memRes.status);
  } catch (e) {
    // 清空失败时把错误显示在对话区，而不是留在看不见的空状态里
    enterChatMode();
    addMsg('ai', '记忆清空失败：' + e.message + '。请稍后重试。');
  } finally {
    newChatBtn.disabled = false;
    if (desktopMedia.matches) input.focus();
  }
}
newChatBtn.onclick = startNewChat;

// ---------- 侧边栏折叠 / 移动端抽屉 ----------
const SIDEBAR_KEY = 'sidebar-collapsed';
const desktopMedia = window.matchMedia('(min-width: 768px)');
const root = document.documentElement;

function readSidebarCollapsed() {
  try { return localStorage.getItem(SIDEBAR_KEY) === '1'; }
  catch (e) { return false; }
}

function saveSidebarCollapsed(collapsed) {
  try { localStorage.setItem(SIDEBAR_KEY, collapsed ? '1' : '0'); }
  catch (e) { /* localStorage 不可用时本次会话仍然生效 */ }
}

function setSidebarCollapsed(collapsed) {
  root.classList.toggle('sidebar-collapsed', collapsed);
  const label = collapsed ? '展开侧边栏' : '收起侧边栏';
  sidebarCollapse.setAttribute('aria-expanded', String(!collapsed));
  sidebarCollapse.setAttribute('aria-label', label);
  sidebarCollapse.title = label;
}

function closeSidebarDrawer() {
  sidebar.classList.remove('open');
  sidebarOverlay.classList.remove('show');
  menuToggle.setAttribute('aria-expanded', 'false');
  menuToggle.setAttribute('aria-label', '打开菜单');
}

function openSidebarDrawer() {
  sidebar.classList.add('open');
  sidebarOverlay.classList.add('show');
  menuToggle.setAttribute('aria-expanded', 'true');
  menuToggle.setAttribute('aria-label', '关闭菜单');
}

// 桌面端恢复记忆状态；手机端始终以隐藏抽屉的形式出现
function syncSidebarMode() {
  if (desktopMedia.matches) {
    setSidebarCollapsed(readSidebarCollapsed());
    closeSidebarDrawer();
  } else {
    root.classList.remove('sidebar-collapsed');
    closeSidebarDrawer();
  }
}

sidebarCollapse.onclick = () => {
  if (!desktopMedia.matches) return;
  const next = !root.classList.contains('sidebar-collapsed');
  setSidebarCollapsed(next);
  saveSidebarCollapsed(next);
};

menuToggle.onclick = () => {
  if (sidebar.classList.contains('open')) closeSidebarDrawer();
  else openSidebarDrawer();
};
sidebarOverlay.onclick = closeSidebarDrawer;
document.querySelectorAll('.nav-item').forEach(a =>
  a.addEventListener('click', closeSidebarDrawer)
);

if (desktopMedia.addEventListener) desktopMedia.addEventListener('change', syncSidebarMode);
else desktopMedia.addListener(syncSidebarMode);

syncSidebarMode();

// ---------- 记忆面板 ----------
const MEM_FIELDS = [
  ['name', '名字'],
  ['school', '学校'],
  ['major', '专业'],
  ['student_union_role', '学生会 / 团委职务'],
  ['class_role', '班级职务'],
  ['current_activities', '最近正在做的事'],
  ['other_facts', '其他长期信息'],
  ['schedule_notes', '日程备注'],
  ['preferences', '偏好与长期画像'],
];

function openMemory() {
  memoryModal.style.display = 'block';
  memoryModal.setAttribute('aria-hidden', 'false');
  requestAnimationFrame(() => memoryModal.classList.add('open'));
  loadMemory();
}
function closeMemory() {
  memoryModal.classList.remove('open');
  memoryModal.setAttribute('aria-hidden', 'true');
  setTimeout(() => {
    if (!memoryModal.classList.contains('open')) memoryModal.style.display = 'none';
  }, 200);
}

async function loadMemory() {
  memoryBody.innerHTML = '<div class="memory-empty">加载中…</div>';
  try {
    const res = await fetch('/api/profile');
    renderMemory(await res.json());
  } catch (e) {
    memoryBody.innerHTML = '<div class="memory-empty">读取失败：' + escapeHtml(e.message) + '</div>';
  }
}

function renderMemory(profile) {
  const entries = MEM_FIELDS.filter(([k]) => {
    const v = profile[k];
    if (v === undefined || v === null || v === '') return false;
    if (Array.isArray(v)) return v.length > 0;
    return true;
  });
  if (!entries.length) {
    memoryBody.innerHTML = '<div class="memory-empty">还没有记住任何信息。<br>试着告诉我你的职务，或最近在做的事。</div>';
    return;
  }
  memoryBody.innerHTML = '';
  entries.forEach(([key, label]) => {
    const val = profile[key];
    const group = document.createElement('div');
    group.className = 'mem-group';
    const lab = document.createElement('div');
    lab.className = 'mem-label';
    lab.textContent = label;
    group.appendChild(lab);
    if (Array.isArray(val)) {
      val.forEach(item => group.appendChild(makeMemItem(key, String(item), true)));
    } else {
      group.appendChild(makeMemItem(key, String(val), false));
    }
    memoryBody.appendChild(group);
  });
}

function makeMemItem(field, text, isList) {
  const item = document.createElement('div');
  item.className = 'mem-item';
  const span = document.createElement('span');
  span.className = 'mem-text';
  span.textContent = text;
  const del = document.createElement('button');
  del.className = 'mem-del';
  del.title = '删除这条记忆';
  del.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6"/></svg>';
  del.onclick = () => deleteMemory(field, isList ? text : '');
  item.appendChild(span);
  item.appendChild(del);
  return item;
}

async function deleteMemory(field, value) {
  try {
    const res = await fetch('/api/profile/delete', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ field, value: value || '' })
    });
    const data = await res.json();
    renderMemory(data.profile || {});
  } catch (e) { /* 忽略 */ }
}

memoryBtn.onclick = openMemory;
memoryClose.onclick = closeMemory;
memoryBackdrop.onclick = closeMemory;
memoryRefresh.onclick = loadMemory;
memoryReset.onclick = async () => {
  if (!confirm('确定清空自动记忆（职务、最近在做的事等）吗？')) return;
  try {
    const res = await fetch('/api/profile/reset', { method: 'POST' });
    renderMemory((await res.json()).profile || {});
  } catch (e) { /* 忽略 */ }
};
document.addEventListener('keydown', e => {
  if (e.key !== 'Escape') return;
  if (memoryModal.classList.contains('open')) closeMemory();
  else if (sidebar.classList.contains('open')) closeSidebarDrawer();
});

// ---------- 移动端可视视口：地址栏伸缩 / 软键盘 ----------
// 键盘弹出时 visualViewport 会明显矮于布局视口，此时用 --vvh 顶住 .app 的高度，
// 输入框不会被顶到屏幕外；其余情况交给 CSS 的 100dvh，避免桌面端 1px 抖动。
(function syncVisualViewport() {
  const vv = window.visualViewport;
  if (!vv) return;
  const rootStyle = document.documentElement.style;
  const KEYBOARD_MIN = 120; // 被遮挡超过这个高度就认为是软键盘
  let frame = 0;

  const apply = () => {
    frame = 0;
    if (vv.scale > 1.01) return; // 双指缩放时不压缩布局
    const covered = window.innerHeight - vv.height - vv.offsetTop;
    if (covered > KEYBOARD_MIN) {
      rootStyle.setProperty('--vvh', `${Math.round(vv.height)}px`);
    } else {
      rootStyle.removeProperty('--vvh');
    }
    // 键盘弹出后把最新消息顶回视野内
    if (covered > KEYBOARD_MIN && document.activeElement === input &&
        messages.scrollHeight > messages.clientHeight) {
      messages.scrollTop = messages.scrollHeight;
    }
  };
  const schedule = () => { if (!frame) frame = requestAnimationFrame(apply); };

  vv.addEventListener('resize', schedule);
  vv.addEventListener('scroll', schedule);
  apply();
})();

// ---------- 初始化 ----------
loadFileState();
renderWelcome();
