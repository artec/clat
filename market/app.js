const state = { packages: [], query: '', filter: 'all' }

const grid = document.querySelector('#package-grid')
const count = document.querySelector('#result-count')
const notice = document.querySelector('#catalog-notice')
const empty = document.querySelector('#empty-state')
const search = document.querySelector('#search')
const dialog = document.querySelector('#package-dialog')
const dialogContent = document.querySelector('#dialog-content')

const icon = (name) => {
  const paths = {
    wasm: '<path d="M5 5h14v14H5zM9 9h6v6H9z"/>',
    mcp: '<circle cx="7" cy="12" r="3"/><circle cx="17" cy="7" r="3"/><circle cx="17" cy="17" r="3"/><path d="m10 11 4-3m-4 5 4 3"/>',
    arrow: '<path d="M5 12h14m-5-5 5 5-5 5"/>',
    shield: '<path d="M12 3 20 7v5c0 5-3.4 8-8 10-4.6-2-8-5-8-10V7z"/><path d="m8.5 12 2.2 2.2 4.8-5"/>'
  }
  return `<svg viewBox="0 0 24 24" aria-hidden="true">${paths[name]}</svg>`
}

function filteredPackages() {
  const query = state.query.toLocaleLowerCase('zh-CN')
  return state.packages.filter((plugin) => {
    if (state.filter !== 'all' && plugin.ecosystem !== state.filter) return false
    const haystack = [plugin.id, plugin.name, plugin.summary, ...(plugin.tags || [])]
      .join(' ')
      .toLocaleLowerCase('zh-CN')
    return !query || haystack.includes(query)
  })
}

function render() {
  const packages = filteredPackages()
  count.textContent = `${packages.length.toString().padStart(2, '0')} / ${state.packages.length
    .toString()
    .padStart(2, '0')} 条目`
  grid.replaceChildren(...packages.map(packageCard))
  empty.hidden = packages.length !== 0
}

function packageCard(plugin, index) {
  const article = document.createElement('article')
  article.className = `package-card accent-${plugin.accent || 'lime'}`
  article.style.setProperty('--index', index)
  article.innerHTML = `
    <div class="card-topline">
      <span class="runtime-icon">${icon(plugin.runtime === 'wasm-component' ? 'wasm' : 'mcp')}</span>
      <span class="package-status status-${plugin.status}">${plugin.status === 'available' ? '可安装' : '预览'}</span>
    </div>
    <p class="package-id"></p>
    <h3></h3>
    <p class="package-summary"></p>
    <div class="tag-list"></div>
    <div class="card-footer">
      <span>${plugin.publisher}</span>
      <button type="button" aria-label="查看 ${plugin.name} 详情">详情 ${icon('arrow')}</button>
    </div>`
  article.querySelector('.package-id').textContent = plugin.id
  article.querySelector('h3').textContent = plugin.name
  article.querySelector('.package-summary').textContent = plugin.summary
  const tags = article.querySelector('.tag-list')
  tags.replaceChildren(...(plugin.tags || []).map((tag) => {
    const span = document.createElement('span')
    span.textContent = tag
    return span
  }))
  article.querySelector('button').addEventListener('click', () => openPackage(plugin))
  return article
}

function openPackage(plugin) {
  const available = plugin.status === 'available'
  dialogContent.replaceChildren()
  const wrapper = document.createElement('div')
  wrapper.innerHTML = `
    <p class="eyebrow">PACKAGE RECORD / ${plugin.runtime.toUpperCase()}</p>
    <div class="dialog-title-row">
      <span class="runtime-icon large">${icon(plugin.runtime === 'wasm-component' ? 'wasm' : 'mcp')}</span>
      <div><p class="package-id"></p><h2 id="dialog-title"></h2></div>
    </div>
    <p class="dialog-summary"></p>
    <dl class="package-facts">
      <div><dt>VERSION</dt><dd>${plugin.latest}</dd></div>
      <div><dt>RUNTIME</dt><dd>${plugin.runtime}</dd></div>
      <div><dt>ECOSYSTEM</dt><dd>${plugin.ecosystem === 'dsh-compatible' ? 'DSH compatible' : 'CLAT native'}</dd></div>
      <div><dt>TRUST</dt><dd>${icon('shield')} ${plugin.trust}</dd></div>
    </dl>
    <div class="capability-block"><b>声明能力</b><div class="tag-list capability-list"></div></div>
    <div class="dialog-actions"></div>`
  wrapper.querySelector('.package-id').textContent = plugin.id
  wrapper.querySelector('h2').textContent = plugin.name
  wrapper.querySelector('.dialog-summary').textContent = plugin.summary
  const capabilities = wrapper.querySelector('.capability-list')
  capabilities.replaceChildren(...(plugin.capabilities || []).map((capability) => {
    const span = document.createElement('span')
    span.textContent = capability
    return span
  }))
  const actions = wrapper.querySelector('.dialog-actions')
  if (available) {
    const command = document.createElement('code')
    command.textContent = `clat plugin market install ${plugin.id} --accept-capabilities`
    actions.append(command)
  } else {
    const pending = document.createElement('p')
    pending.className = 'preview-note'
    pending.textContent = '该条目正在完成发布者与制品复核，目前仅展示源代码和兼容方向。'
    actions.append(pending)
  }
  for (const [label, url] of [['查看源代码', plugin.sourceUrl], ['阅读文档', plugin.docsUrl]]) {
    if (!url) continue
    const link = document.createElement('a')
    link.href = url
    link.target = '_blank'
    link.rel = 'noopener noreferrer'
    link.textContent = label
    actions.append(link)
  }
  dialogContent.append(wrapper)
  dialog.showModal()
}

search.addEventListener('input', () => {
  state.query = search.value.trim()
  render()
})

document.querySelectorAll('.filter').forEach((button) => {
  button.addEventListener('click', () => {
    state.filter = button.dataset.filter
    document.querySelectorAll('.filter').forEach((item) => item.classList.toggle('is-active', item === button))
    render()
  })
})

document.addEventListener('keydown', (event) => {
  if (event.key === '/' && document.activeElement !== search) {
    event.preventDefault()
    search.focus()
  }
})

document.querySelector('.dialog-close').addEventListener('click', () => dialog.close())
dialog.addEventListener('click', (event) => {
  if (event.target === dialog) dialog.close()
})

try {
  const response = await fetch('./catalog.json', { credentials: 'omit', cache: 'no-cache' })
  if (!response.ok) throw new Error(`HTTP ${response.status}`)
  const catalog = await response.json()
  if (catalog.schemaVersion !== 1 || !Array.isArray(catalog.packages)) throw new Error('catalog schema')
  state.packages = catalog.packages
  notice.textContent = catalog.market?.notice || ''
  render()
} catch (error) {
  count.textContent = '目录暂时不可用'
  notice.textContent = '无法读取 catalog.json，请稍后重试。'
  empty.hidden = false
  console.error('catalog load failed', error)
}
