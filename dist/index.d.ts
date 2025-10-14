import { type Plugin } from "vite";
export type StepAction = string | (() => void | Promise<void>);
export type StepSpec = {
  name: string;
  action: StepAction;
};
export declare const Step: (spec: StepSpec) => StepSpec;
export interface ApxPluginOptions {
  steps?: StepSpec[];
  ignore?: string[];
}
export declare function apx(options?: ApxPluginOptions): Plugin;
export default apx;
//# sourceMappingURL=index.d.ts.map
