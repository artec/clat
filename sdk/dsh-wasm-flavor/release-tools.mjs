// Product surface for the first exact-origin release, independent of original sources.
export function releaseDefinitions(definitions, searchOnly) {
  return searchOnly ? definitions.filter(tool => tool.name === 'web_search') : definitions
}
export function requireReleaseTool(name, searchOnly) {
  if (searchOnly && name !== 'web_search') throw new Error('tool unavailable in search-only release')
}
