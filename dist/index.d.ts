import { type Plugin } from "vite";
import { type OptionsExport as OrvalConfig } from "orval";
export type { OrvalConfig };
export type StepAction = string | (() => void | Promise<void>);
export type StepSpec = {
    name: string;
    action: StepAction;
};
export declare const Step: (spec: StepSpec) => StepSpec;
/**
 * Predefined step for generating OpenAPI schema
 * @param appModule - The Python module path (e.g., "sample.api.app:app")
 * @param outputPath - Where to write the OpenAPI JSON file
 */
export declare const OpenAPI: (appModule: string, outputPath: string) => StepSpec;
/**
 * Predefined step for generating API client with Orval
 * @param config - Orval configuration object
 */
export declare const Orval: (config: OrvalConfig) => StepSpec;
export interface ApxPluginOptions {
    steps?: StepSpec[];
    ignore?: string[];
}
export declare function apx(options?: ApxPluginOptions): Plugin;
export default apx;
//# sourceMappingURL=index.d.ts.map