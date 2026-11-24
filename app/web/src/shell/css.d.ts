// Stylesheets are imported for their side effect only (`import "./shell.css";`); Vite bundles them.
// TypeScript 7 rejects a side-effect import it can't resolve to a module (TS2882), so declare one.
declare module "*.css";
