import asyncio
import contextlib
import logging
import threading
from types import SimpleNamespace

import babeldoc
import pytest
from babeldoc.asynchronize import AsyncCallback
from babeldoc.format.pdf import high_level
from babeldoc.progress_monitor import ProgressMonitor


@pytest.fixture
def runtime(monkeypatch, tmp_path):
    state = SimpleNamespace(
        actions=[],
        cleanup_started=threading.Event(),
        release_cleanup=threading.Event(),
        cleanup_error=None,
    )
    state.release_cleanup.set()

    class Context:
        def close(self):
            state.actions.append("context-start")
            state.cleanup_started.set()
            if not state.release_cleanup.wait(5):
                raise RuntimeError("test did not release cleanup")
            if state.cleanup_error is not None:
                raise state.cleanup_error
            state.actions.append("context-end")

    class MemoryMonitor:
        peak_memory_usage = 0

        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return None

    def record(action):
        state.actions.append(action)

    native = SimpleNamespace(
        acquire_logging_run=lambda: record("acquire"),
        flush_logging_run=lambda: record("flush"),
        stop_logging_run=lambda: record("stop"),
    )
    state.config = SimpleNamespace(
        input_file=tmp_path / "input.pdf",
        report_interval=0.01,
        experimental_cross_page_context=False,
        split_strategy=None,
        deterministic_output=False,
        debug=False,
        shared_context_cross_split_part=SimpleNamespace(),
        cleanup_temp_files=lambda: record("temp"),
    )
    state.result = SimpleNamespace()
    monkeypatch.setattr(babeldoc, "_rust_il", native, raising=False)
    monkeypatch.setattr(
        high_level, "_require_stage_capture_feature", lambda _config: None
    )
    monkeypatch.setattr(
        high_level, "get_translation_stage", lambda _config: [("stage", 1)]
    )
    monkeypatch.setattr(high_level, "Document", lambda _path: object())
    monkeypatch.setattr(high_level, "check_metadata", lambda _document: None)
    monkeypatch.setattr(
        high_level, "create_document_run_context", lambda _config: Context()
    )
    monkeypatch.setattr(high_level, "MemoryMonitor", MemoryMonitor)
    monkeypatch.setattr(high_level, "_do_translate_single", lambda *_args: state.result)
    monkeypatch.setattr(high_level, "fix_cmap", lambda *_args: None)
    monkeypatch.setattr(high_level, "add_metadata", lambda *_args: None)
    monkeypatch.setattr(high_level, "migrate_toc", lambda *_args: None)
    return state


async def wait_for_thread_event(event):
    assert await asyncio.to_thread(event.wait, 2)


def test_callback_drains_ordered_events_and_one_terminal():
    async def run():
        callback = AsyncCallback()

        async def consume():
            return [event.kwargs["type"] async for event in callback]

        consumer = asyncio.create_task(consume())
        await asyncio.sleep(0)
        await asyncio.to_thread(callback.step_callback, type="progress")
        await asyncio.to_thread(callback.finished_callback, type="finish")
        await asyncio.to_thread(callback.finished_callback, type="error")
        await asyncio.to_thread(callback.step_callback, type="late")
        assert await asyncio.wait_for(consumer, 1) == ["progress", "finish"]
        with pytest.raises(StopAsyncIteration):
            await callback.__anext__()

    asyncio.run(run())


def test_concurrent_terminal_callbacks_have_one_winner():
    async def run():
        callback = AsyncCallback()
        barrier = threading.Barrier(3)

        def finish(kind):
            barrier.wait()
            callback.finished_callback(type=kind)

        first = asyncio.create_task(asyncio.to_thread(finish, "finish"))
        second = asyncio.create_task(asyncio.to_thread(finish, "error"))
        await asyncio.to_thread(barrier.wait)
        await asyncio.gather(first, second)
        events = [event.kwargs["type"] async for event in callback]
        assert len(events) == 1
        assert events[0] in ("finish", "error")

    asyncio.run(run())


def test_finish_follows_cleanup_and_worker_return(runtime, monkeypatch):
    runtime.release_cleanup.clear()
    actual_do_translate = high_level.do_translate

    def tracked_do_translate(*args):
        try:
            return actual_do_translate(*args)
        finally:
            runtime.actions.append("worker-return")

    monkeypatch.setattr(high_level, "do_translate", tracked_do_translate)

    async def run():
        async def consume():
            return [event async for event in high_level.async_translate(runtime.config)]

        consumer = asyncio.create_task(consume())
        try:
            await wait_for_thread_event(runtime.cleanup_started)
            await asyncio.sleep(0)
            assert not consumer.done()
        finally:
            runtime.release_cleanup.set()
        events = await asyncio.wait_for(consumer, 2)
        assert [event["type"] for event in events] == ["stage_summary", "finish"]
        assert events[-1]["translate_result"] is runtime.result
        assert runtime.actions[-5:] == [
            "context-end",
            "flush",
            "stop",
            "temp",
            "worker-return",
        ]

    asyncio.run(run())


def test_cleanup_failure_is_terminal_error(runtime):
    runtime.cleanup_error = RuntimeError("cleanup failed")

    async def run():
        loop_errors = []
        asyncio.get_running_loop().set_exception_handler(
            lambda _loop, context: loop_errors.append(context)
        )
        events = [event async for event in high_level.async_translate(runtime.config)]
        assert [event["type"] for event in events] == ["stage_summary", "error"]
        assert events[-1]["error"] is runtime.cleanup_error
        assert runtime.actions[-3:] == ["flush", "stop", "temp"]
        await asyncio.sleep(0)
        assert not loop_errors

    asyncio.run(run())


@pytest.mark.parametrize("failure_point", ["flush", "stop", "temp"])
def test_each_cleanup_failure_is_terminal_error(runtime, monkeypatch, failure_point):
    error = OSError(f"{failure_point} failed")

    def fail():
        runtime.actions.append(failure_point)
        raise error

    if failure_point == "temp":
        runtime.config.cleanup_temp_files = fail
    else:
        monkeypatch.setattr(babeldoc._rust_il, f"{failure_point}_logging_run", fail)

    async def run():
        events = [event async for event in high_level.async_translate(runtime.config)]
        assert [event["type"] for event in events] == ["stage_summary", "error"]
        assert events[-1]["error"] is error
        assert runtime.actions[-4:] == ["context-end", "flush", "stop", "temp"]

    asyncio.run(run())


def test_async_error_follows_cleanup_failure_report(runtime, monkeypatch, tmp_path):
    runtime.config.experimental_cross_page_context = True
    runtime.config.only_parse_generate_pdf = False
    runtime.config.skip_translation = False
    runtime.config.no_mono = True
    runtime.config.no_dual = True
    runtime.result.mono_pdf_path = None
    runtime.result.dual_pdf_path = None
    runtime.config.get_working_file_path = lambda _name: tmp_path / "report.json"
    runtime.cleanup_error = RuntimeError("cleanup failed")

    def persist(_config, *_args, **kwargs):
        if kwargs.get("error") is not None:
            runtime.actions.append("failure-report")

    monkeypatch.setattr(high_level, "_persist_cross_boundary_report", persist)

    async def run():
        events = [event async for event in high_level.async_translate(runtime.config)]
        assert [event["type"] for event in events] == ["stage_summary", "error"]
        assert events[-1]["error"] is runtime.cleanup_error
        assert runtime.actions[-1] == "failure-report"

    asyncio.run(run())


def test_worker_cancellation_with_failed_cleanup_is_error_event(runtime, monkeypatch):
    runtime.cleanup_error = RuntimeError("cleanup after cancellation failed")

    def cancel(*_args):
        raise asyncio.CancelledError()

    monkeypatch.setattr(high_level, "_do_translate_single", cancel)

    async def run():
        events = [event async for event in high_level.async_translate(runtime.config)]
        assert [event["type"] for event in events] == ["stage_summary", "error"]
        error = events[-1]["error"]
        assert error is runtime.cleanup_error
        assert isinstance(error.__cause__, asyncio.CancelledError)
        assert error.__cause__.__cause__ is None

    asyncio.run(run())


def test_synchronous_error_event_follows_cleanup(runtime, monkeypatch):
    failure = RuntimeError("translation failed")
    events = []

    def fail(*_args):
        raise failure

    monkeypatch.setattr(high_level, "_do_translate_single", fail)
    monitor = ProgressMonitor(
        [("stage", 1)],
        finish_callback=lambda **event: events.append((event, list(runtime.actions))),
    )
    with pytest.raises(RuntimeError, match="translation failed"):
        high_level.do_translate(monitor, runtime.config)
    assert events == [({"type": "error", "error": failure}, runtime.actions)]
    assert runtime.actions[-4:] == ["context-end", "flush", "stop", "temp"]


def test_progress_monitor_finish_signals_completion_without_cancelling():
    async def run():
        finish_event = asyncio.Event()
        cancel_event = threading.Event()
        events = []
        monitor = ProgressMonitor(
            [("stage", 1)],
            finish_callback=lambda **event: events.append(event),
            finish_event=finish_event,
            cancel_event=cancel_event,
            loop=asyncio.get_running_loop(),
        )
        await asyncio.to_thread(monitor.on_finish)
        await asyncio.wait_for(finish_event.wait(), 1)
        assert not cancel_event.is_set()
        assert not events

    asyncio.run(run())


def test_synchronous_cleanup_failure_preserves_cancellation_and_cause(
    runtime, monkeypatch
):
    runtime.cleanup_error = RuntimeError("cleanup failed after cancellation")

    def cancel(*_args):
        raise asyncio.CancelledError()

    monkeypatch.setattr(high_level, "_do_translate_single", cancel)
    monitor = ProgressMonitor([("stage", 1)])
    with pytest.raises(asyncio.CancelledError) as caught:
        high_level.do_translate(monitor, runtime.config)
    assert caught.value.__cause__ is runtime.cleanup_error


def test_aclose_cancels_and_waits_for_cleanup(runtime, monkeypatch):
    runtime.release_cleanup.clear()
    started = threading.Event()

    def translate(monitor, *_args):
        started.set()
        if monitor.cancel_event.wait(4):
            raise asyncio.CancelledError()
        raise RuntimeError("cancellation was not requested")

    monkeypatch.setattr(high_level, "_do_translate_single", translate)

    async def run():
        events = high_level.async_translate(runtime.config)
        assert (await events.__anext__())["type"] == "stage_summary"
        await wait_for_thread_event(started)
        closer = asyncio.create_task(events.aclose())
        try:
            await wait_for_thread_event(runtime.cleanup_started)
            assert not closer.done()
        finally:
            runtime.config.progress_monitor.cancel_event.set()
            runtime.release_cleanup.set()
        await asyncio.wait_for(closer, 2)
        assert runtime.actions[-4:] == ["context-end", "flush", "stop", "temp"]

    asyncio.run(run())


def test_explicit_break_closes_stream_and_waits(runtime, monkeypatch):
    runtime.release_cleanup.clear()
    started = threading.Event()
    proceed = asyncio.Event()

    def translate(monitor, *_args):
        started.set()
        if monitor.cancel_event.wait(4):
            raise asyncio.CancelledError()
        raise RuntimeError("cancellation was not requested")

    monkeypatch.setattr(high_level, "_do_translate_single", translate)

    async def run():
        async def consume():
            async with contextlib.aclosing(
                high_level.async_translate(runtime.config)
            ) as events:
                async for event in events:
                    if event["type"] == "stage_summary":
                        await proceed.wait()
                        break

        consumer = asyncio.create_task(consume())
        await wait_for_thread_event(started)
        proceed.set()
        try:
            await wait_for_thread_event(runtime.cleanup_started)
            assert not consumer.done()
        finally:
            runtime.config.progress_monitor.cancel_event.set()
            runtime.release_cleanup.set()
        await asyncio.wait_for(consumer, 2)

    asyncio.run(run())


def test_aclose_exposes_cleanup_failure_after_translation_error(runtime, monkeypatch):
    runtime.release_cleanup.clear()
    runtime.cleanup_error = RuntimeError("cleanup failed")
    started = threading.Event()
    translation_error = ValueError("translation failed")

    def translate(monitor, *_args):
        started.set()
        monitor.cancel_event.wait(4)
        raise translation_error

    monkeypatch.setattr(high_level, "_do_translate_single", translate)

    async def run():
        events = high_level.async_translate(runtime.config)
        await events.__anext__()
        await wait_for_thread_event(started)
        closer = asyncio.create_task(events.aclose())
        try:
            await wait_for_thread_event(runtime.cleanup_started)
            assert not closer.done()
        finally:
            runtime.config.progress_monitor.cancel_event.set()
            runtime.release_cleanup.set()
        with pytest.raises(RuntimeError, match="cleanup failed") as caught:
            await asyncio.wait_for(closer, 2)
        assert caught.value.__cause__ is translation_error

    asyncio.run(run())


def test_aclose_exposes_cleanup_failure_after_cancellation(runtime, monkeypatch):
    runtime.release_cleanup.clear()
    runtime.cleanup_error = RuntimeError("cleanup after cancellation failed")
    started = threading.Event()

    def translate(monitor, *_args):
        started.set()
        monitor.cancel_event.wait(4)
        raise asyncio.CancelledError()

    monkeypatch.setattr(high_level, "_do_translate_single", translate)

    async def run():
        events = high_level.async_translate(runtime.config)
        await events.__anext__()
        await wait_for_thread_event(started)
        closer = asyncio.create_task(events.aclose())
        try:
            await wait_for_thread_event(runtime.cleanup_started)
            assert not closer.done()
        finally:
            runtime.config.progress_monitor.cancel_event.set()
            runtime.release_cleanup.set()
        with pytest.raises(
            RuntimeError, match="cleanup after cancellation failed"
        ) as caught:
            await asyncio.wait_for(closer, 2)
        assert isinstance(caught.value.__cause__, asyncio.CancelledError)

    asyncio.run(run())


def test_unexpected_worker_exception_is_delivered(runtime, monkeypatch):
    error = RuntimeError("worker failed before setup")

    def fail(*_args):
        raise error

    monkeypatch.setattr(high_level, "do_translate", fail)

    async def run():
        async def collect():
            return [event async for event in high_level.async_translate(runtime.config)]

        events = await asyncio.wait_for(collect(), 2)
        assert [event["type"] for event in events] == ["stage_summary", "error"]
        assert events[-1]["error"] is error

    asyncio.run(run())


def test_unexpected_worker_cancellation_is_error_event(runtime, monkeypatch):
    def cancel(*_args):
        raise asyncio.CancelledError("worker cancelled")

    monkeypatch.setattr(high_level, "do_translate", cancel)

    async def run():
        events = [event async for event in high_level.async_translate(runtime.config)]
        assert [event["type"] for event in events] == ["stage_summary", "error"]
        assert isinstance(events[-1]["error"], asyncio.CancelledError)

    asyncio.run(run())


def test_break_at_terminal_event_needs_no_additional_worker_wait(runtime):
    async def run():
        async with contextlib.aclosing(
            high_level.async_translate(runtime.config)
        ) as events:
            async for event in events:
                if event["type"] == "finish":
                    break
        assert runtime.actions[-4:] == ["context-end", "flush", "stop", "temp"]
        assert not runtime.config.progress_monitor.cancel_event.is_set()

    asyncio.run(run())


def test_unclosed_break_still_observes_late_worker_failure(
    runtime, monkeypatch, caplog
):
    started = threading.Event()
    release_worker = threading.Event()
    worker_done = threading.Event()
    failure = RuntimeError("late worker failure")

    def fail(*_args):
        started.set()
        if not release_worker.wait(4):
            raise RuntimeError("test did not release worker")
        worker_done.set()
        raise failure

    monkeypatch.setattr(high_level, "do_translate", fail)
    caplog.set_level(logging.ERROR, logger=high_level.__name__)

    async def run():
        loop_errors = []
        asyncio.get_running_loop().set_exception_handler(
            lambda _loop, context: loop_errors.append(context)
        )
        events = high_level.async_translate(runtime.config)
        try:
            async for event in events:
                assert event["type"] == "stage_summary"
                break
            await wait_for_thread_event(started)
            release_worker.set()
            await wait_for_thread_event(worker_done)
            await asyncio.sleep(0.05)
            assert not loop_errors
            assert any(
                record.exc_info and record.exc_info[1] is failure
                for record in caplog.records
            )
        finally:
            release_worker.set()
            with contextlib.suppress(RuntimeError):
                await events.aclose()

    asyncio.run(run())


def test_task_cancellation_waits_through_repeated_cancellation(runtime, monkeypatch):
    runtime.release_cleanup.clear()
    started = threading.Event()

    def translate(monitor, *_args):
        started.set()
        if monitor.cancel_event.wait(4):
            raise asyncio.CancelledError()
        raise RuntimeError("cancellation was not requested")

    monkeypatch.setattr(high_level, "_do_translate_single", translate)

    async def run():
        ready = asyncio.Event()

        async def consume():
            async with contextlib.aclosing(
                high_level.async_translate(runtime.config)
            ) as events:
                async for event in events:
                    if event["type"] == "stage_summary":
                        ready.set()

        consumer = asyncio.create_task(consume())
        await ready.wait()
        await wait_for_thread_event(started)
        consumer.cancel()
        try:
            await wait_for_thread_event(runtime.cleanup_started)
            consumer.cancel()
            await asyncio.sleep(0)
            assert not consumer.done()
        finally:
            runtime.config.progress_monitor.cancel_event.set()
            runtime.release_cleanup.set()
        with pytest.raises(asyncio.CancelledError):
            await asyncio.wait_for(consumer, 2)
        assert runtime.actions[-4:] == ["context-end", "flush", "stop", "temp"]

    asyncio.run(run())


def test_cancelled_consumer_receives_cleanup_failure(runtime, monkeypatch):
    runtime.release_cleanup.clear()
    runtime.cleanup_error = RuntimeError("cleanup after task cancellation failed")
    started = threading.Event()

    def translate(monitor, *_args):
        started.set()
        if monitor.cancel_event.wait(4):
            raise asyncio.CancelledError()
        raise RuntimeError("cancellation was not requested")

    monkeypatch.setattr(high_level, "_do_translate_single", translate)

    async def run():
        ready = asyncio.Event()

        async def consume():
            async with contextlib.aclosing(
                high_level.async_translate(runtime.config)
            ) as events:
                async for event in events:
                    if event["type"] == "stage_summary":
                        ready.set()

        consumer = asyncio.create_task(consume())
        await ready.wait()
        await wait_for_thread_event(started)
        consumer.cancel()
        try:
            await wait_for_thread_event(runtime.cleanup_started)
            assert not consumer.done()
        finally:
            runtime.config.progress_monitor.cancel_event.set()
            runtime.release_cleanup.set()
        with pytest.raises(
            RuntimeError, match="cleanup after task cancellation failed"
        ) as caught:
            await asyncio.wait_for(consumer, 2)
        assert isinstance(caught.value.__cause__, asyncio.CancelledError)

    asyncio.run(run())
