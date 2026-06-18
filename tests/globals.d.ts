// Minimal ambient declarations so TypeScript resolves mocha globals and the
// chai module without requiring @types/mocha and @types/chai to be installed.
// Replace with the real @types packages once `npm install` can be run.

declare function describe(name: string, fn: () => void): void;
declare function it(name: string, fn: () => void | Promise<void>): void;
declare function before(fn: () => void | Promise<void>): void;
declare function beforeEach(fn: () => void | Promise<void>): void;
declare function afterEach(fn: () => void | Promise<void>): void;
declare function after(fn: () => void | Promise<void>): void;

declare module "chai" {
  export function expect(actual: unknown, message?: string): any;
}
