import { type Plugin } from "vite";
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
export interface OrvalOutputOptions {
    target: string;
    baseUrl?: string;
    client?: "react-query" | "swr" | "vue-query" | "svelte-query" | "fetch" | "axios" | "angular";
    httpClient?: "fetch" | "axios";
    prettier?: boolean;
    override?: {
        query?: {
            useQuery?: boolean;
            useSuspenseQuery?: boolean;
            [key: string]: any;
        };
        [key: string]: any;
    };
    [key: string]: any;
}
/**
 * Predefined step for generating API client with Orval
 * Skips generation if the OpenAPI spec hasn't changed since last run
 * @param input - Path to the OpenAPI spec file
 * @param output - Orval output configuration
 */
export declare const Orval: (input: string, output: OrvalOutputOptions) => StepSpec;
export interface ApxPluginOptions {
    steps?: StepSpec[];
    ignore?: string[];
}
export declare function apx(options?: ApxPluginOptions): Plugin;
export default apx;
//# sourceMappingURL=index.d.ts.map