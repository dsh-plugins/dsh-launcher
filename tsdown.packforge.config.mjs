export default {
  entry: ['src-tauri/packforge/worker.mjs'],
  outDir: 'src-tauri/packforge/generated',
  format: 'cjs',
  platform: 'node',
  target: 'node20',
  dts: false,
  noExternal: [/.*/],
  outputOptions: { entryFileNames: 'worker.cjs', inlineDynamicImports: true },
}
