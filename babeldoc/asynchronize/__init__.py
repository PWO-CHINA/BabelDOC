import asyncio
import threading


class Args:
    def __init__(self, args, kwargs):
        self.args = args
        self.kwargs = kwargs


class AsyncCallback:
    def __init__(self):
        self.queue = asyncio.Queue()
        self.finished = False
        self.loop = asyncio.get_event_loop()
        self._lock = threading.Lock()
        self._sentinel = object()
        self._consumed_sentinel = False

    def step_callback(self, *args, **kwargs):
        with self._lock:
            if not self.finished:
                self.loop.call_soon_threadsafe(
                    self.queue.put_nowait, Args(args, kwargs)
                )

    def finished_callback(self, *args, **kwargs):
        with self._lock:
            if self.finished:
                return
            self.finished = True
            self.loop.call_soon_threadsafe(self._enqueue_finish, Args(args, kwargs))

    def _enqueue_finish(self, event):
        self.queue.put_nowait(event)
        self.queue.put_nowait(self._sentinel)

    def __await__(self):
        return self.__anext__().__await__()

    def __aiter__(self):
        return self

    async def __anext__(self):
        if self._consumed_sentinel:
            raise StopAsyncIteration
        result = await self.queue.get()
        if result is self._sentinel:
            self._consumed_sentinel = True
            raise StopAsyncIteration
        return result
