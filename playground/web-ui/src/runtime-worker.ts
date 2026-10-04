import { describeRuntimeLoadFailure, initializeRuntime } from './runtime';

type Request = { id: number; action: 'run'; source: string };
type Reply = { id: number; result?: unknown; error?: string };

const workerScope = globalThis as unknown as {
  addEventListener: (type: 'message', listener: (event: MessageEvent<Request>) => void) => void;
  postMessage: (reply: Reply) => void;
};

let runtimePromise: ReturnType<typeof initializeRuntime> | undefined;
workerScope.addEventListener('message', async ({ data }) => {
  try {
    runtimePromise ??= initializeRuntime();
    const runtime = await runtimePromise;
    workerScope.postMessage({ id: data.id, result: await runtime.run(data.source) });
  } catch (error) {
    workerScope.postMessage({
      id: data.id,
      error: describeRuntimeLoadFailure(error),
    });
  }
});
