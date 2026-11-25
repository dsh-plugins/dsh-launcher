export default [
  ['client.ts', 'client.js', '@dsh-launcher/notifications'],
  ['sidebar.tsx', 'sidebar.js', '@dsh-launcher/sidebar-browser'],
].map(([source, output, id]) => ({
  entry: [`src-tauri/bridge/${source}`],
  outDir: 'src-tauri/bridge/generated',
  format: 'cjs',
  platform: 'browser',
  target: 'es2022',
  dts: false,
  clean: false,
  external: ['react', 'react/jsx-runtime', '@deepseek-ai/dsh-client-store'],
  tsconfig: 'tsconfig.bridge.json',
  outputOptions: {
    entryFileNames: output,
    banner: `window.__ModuleLoader__.load({ id: "${id}", factory: (require) => {`,
    intro: 'var module = { exports: {} }; var exports = module.exports;',
    footer: 'return module.exports; } });',
  },
}))
