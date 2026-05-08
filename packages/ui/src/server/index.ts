export interface ServerOptions {
  files: string[];
  port: number;
  open: boolean;
}

export async function startServer(_options: ServerOptions): Promise<void> {
  throw new Error("TODO: implement");
}
