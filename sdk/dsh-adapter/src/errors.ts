/** Adapter error taxonomy: stable codes, thrown back into plugin code. */
export class AdapterError extends Error {
  readonly code: string

  constructor(code: string, message: string) {
    super(message)
    this.name = 'AdapterError'
    this.code = code
  }
}

/** Format any thrown value the way tool results report it. */
export function errorMessage(error: unknown): string {
  if (error instanceof Error) {
    const code = (error as { code?: unknown }).code
    return typeof code === 'string' && code !== '' ? `[${code}] ${error.message}` : error.message
  }
  return String(error)
}
