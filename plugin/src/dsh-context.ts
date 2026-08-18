/** Structural DSH interfaces: enough of the Harness surface to register against, no runtime import. */

export interface ToolExecutionLike {
  callId?: unknown
  signal?: AbortSignal
}

export interface ToolDefinitionLike {
  name: string
  description: string
  parameters: Record<string, unknown>
  output: {
    schema: Record<string, unknown>
    render(args: unknown, value: unknown): Array<{ type: 'text'; text: string }>
  }
  execute(args: unknown, execution: ToolExecutionLike): Promise<unknown>
  isConcurrencySafe?(args: unknown): boolean
}

export interface ToolRegistryLike {
  register(definition: ToolDefinitionLike): () => void
}

export interface LoggerLike {
  info(message: string, ...args: unknown[]): void
  warn(message: string, ...args: unknown[]): void
  error(message: string, ...args: unknown[]): void
}

export interface MonitorHostContext {
  tools: ToolRegistryLike
  logger(name: string): LoggerLike
  effect(callback: () => (() => void | Promise<void>), label?: string): unknown
}
