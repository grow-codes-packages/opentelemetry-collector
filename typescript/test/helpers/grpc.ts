import {
  Server,
  ServerCredentials,
  status as GrpcStatus,
  type Metadata,
  type ServerUnaryCall,
  type sendUnaryData,
  type UntypedServiceImplementation,
} from "@grpc/grpc-js";

/** One call seen by the mock gRPC server. */
export interface GrpcCall {
  path: string;
  metadata: Metadata;
  body: Buffer;
}

/** Reply of the mock handler: bytes for success or a status error. */
export type GrpcReply = { body?: Buffer; code?: number; details?: string; delayMs?: number };

export interface MockGrpcServer {
  url: string;
  calls: GrpcCall[];
  close(): Promise<void>;
}

const PATHS = {
  traces: "/opentelemetry.proto.collector.trace.v1.TraceService/Export",
  logs: "/opentelemetry.proto.collector.logs.v1.LogsService/Export",
  metrics: "/opentelemetry.proto.collector.metrics.v1.MetricsService/Export",
};

const identity = (b: Buffer): Buffer => b;

/** Starts an h2c gRPC server serving the three OTLP Export methods. */
export async function startGrpcServer(
  handler: (call: GrpcCall, index: number) => GrpcReply = () => ({}),
): Promise<MockGrpcServer> {
  const server = new Server();
  const calls: GrpcCall[] = [];
  for (const [name, path] of Object.entries(PATHS)) {
    const impl: UntypedServiceImplementation = {
      Export: (call: ServerUnaryCall<Buffer, Buffer>, cb: sendUnaryData<Buffer>) => {
        const recorded = { path, metadata: call.metadata, body: call.request };
        const index = calls.length;
        calls.push(recorded);
        const reply = handler(recorded, index);
        const finish = (): void => {
          if (reply.code !== undefined && reply.code !== 0)
            cb({ code: reply.code as unknown as GrpcStatus, details: reply.details ?? "" }, null);
          else cb(null, reply.body ?? Buffer.alloc(0));
        };
        if (reply.delayMs !== undefined) setTimeout(finish, reply.delayMs);
        else finish();
      },
    };
    server.addService(
      {
        Export: {
          path,
          requestStream: false,
          responseStream: false,
          requestSerialize: identity,
          requestDeserialize: identity,
          responseSerialize: identity,
          responseDeserialize: identity,
          originalName: `export${name}`,
        },
      },
      impl,
    );
  }
  const port = await new Promise<number>((resolve, reject) => {
    server.bindAsync("127.0.0.1:0", ServerCredentials.createInsecure(), (err, p) => {
      if (err) reject(err);
      else resolve(p);
    });
  });
  return {
    url: `http://127.0.0.1:${port}`,
    calls,
    close: () =>
      new Promise<void>((resolve) => {
        server.tryShutdown(() => resolve());
        setTimeout(() => {
          server.forceShutdown();
          resolve();
        }, 200).unref();
      }),
  };
}
