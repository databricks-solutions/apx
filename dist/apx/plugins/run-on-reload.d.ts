import { type Plugin } from "vite";
export type StepAction = () => void | Promise<void>;
export type StepSpec = {
    name: string;
    action: StepAction;
};
export declare const Step: (s: StepSpec) => StepSpec;
export declare function runOnReload({ steps, ignore, }: {
    steps: StepSpec[];
    ignore?: string[];
}): Plugin;
//# sourceMappingURL=run-on-reload.d.ts.map