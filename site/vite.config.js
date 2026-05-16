import { defineConfig } from "vite";
import { createHtmlPlugin } from "vite-plugin-html";

export default defineConfig({
  root: ".",
  base: "./",
  build: {
    outDir: "dist",
    emptyOutDir: true,
    minify: "terser",
    terserOptions: {
      compress: {
        drop_console: false,
        drop_debugger: true,
      },
    },
    assetsInlineLimit: 0,
  },
  plugins: [
    createHtmlPlugin({
      minify: {
        collapseWhitespace: true,
        removeComments: true,
        minifyJS: true,
        minifyCSS: true,
        keepClosingSlash: false,
        removeRedundantAttributes: true,
        useShortDoctype: true,
      },
    }),
  ],
});
