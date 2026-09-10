// 本地焦点事件：⌘N / 顶栏菜单 / 形态切换后请求录入框或列表焦点。

export type FocusTarget = 'compose'

export function requestFocus() {
  window.dispatchEvent(new Event('doing://focus-request'))
}

/** 监听焦点请求（返回取消函数）。 */
export function onFocusRequest(handler: () => void): () => void {
  const fn = () => handler()
  window.addEventListener('doing://focus-request', fn)
  return () => window.removeEventListener('doing://focus-request', fn)
}
