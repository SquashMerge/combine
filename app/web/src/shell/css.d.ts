// Stylesheets are imported for their side effect only (`import "./shell.css";`), and SVG files for
// their URL (`import logoUrl from "./combine-icon-small.svg";`); Vite bundles both. TypeScript 7
// rejects an import it can't resolve to a module (TS2882), so declare them.
declare module "*.css";
declare module "*.svg" {
  const url: string;
  export default url;
}
