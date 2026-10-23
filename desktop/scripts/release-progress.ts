import type { Environment } from './common';

type Status = 'start' | 'done' | 'failed' | 'reused';
const markers: Record<Status, [string, string]> = {
  start: ['⟳', '36'], done: ['✓', '32'], failed: ['✗', '31'], reused: ['↺', '33'],
};
export function useColor(env: Environment = process.env, tty = !!process.stdout.isTTY): boolean {
  if (env.NO_COLOR !== undefined || env.FORCE_COLOR === '0' || env.TERM === 'dumb') return false;
  return tty || (env.FORCE_COLOR !== undefined && env.FORCE_COLOR !== '');
}

// Formatting only: signing, persistence, retries and execution order stay with their owners.
export class ReleaseProgress {
  constructor(readonly output: (line: string) => void = line => console.log(line), readonly color = useColor()) {}
  paint(text: string, code: string): string { return this.color ? `\u001b[${code}m${text}\u001b[0m` : text; }
  time(text: string): string { return this.paint(text, '1;33'); }
  line(status: Status, text: string): void {
    const [symbol, code] = markers[status];
    this.output(`\n${this.paint(symbol, code)} ${text}`);
  }
  summary(text: string): void { this.output(`\n${text}`); }
  async step<T>(label: string, action: () => Promise<T>): Promise<T> {
    this.line('start', label);
    try {
      const result = await action();
      this.line('done', label + '完成');
      return result;
    } catch (error) {
      this.line('failed', label + '失败');
      throw error;
    }
  }
}
