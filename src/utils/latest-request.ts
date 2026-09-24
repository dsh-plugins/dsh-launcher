/** Invalidates pending asynchronous reads when their selection changes. */
export function latestRequest() {
  let generation = 0
  return {
    begin: () => ++generation,
    invalidate: () => { generation++ },
    isCurrent: (request: number) => request === generation,
  }
}
