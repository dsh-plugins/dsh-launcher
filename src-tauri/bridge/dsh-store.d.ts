declare module '@deepseek-ai/dsh-client-store' {
  /** DSH 按会话保存插槽状态，并将同一份记录提供给标签正文和标题。 */
  export function defineStore<S, A>(definition: {
    init: () => S
    persist: string
    actions: A
  }): unknown
}
