# croft's Jupyter kernel bridge (#355).
#
# croft speaks JSON lines to this script on stdin/stdout; the script speaks
# the Jupyter wire protocol to one kernel through jupyter_client, which every
# ipykernel install already carries. Outputs are sent in nbformat v4 shape,
# so croft stores them in the notebook exactly as they arrive.
#
#   in:  {"op": "execute", "id": "<cell>", "code": "..."}
#        {"op": "interrupt"} | {"op": "restart"} | {"op": "shutdown"}
#   out: {"ev": "ready", "kernel": "python3", "display": "Python 3 (ipykernel)"}
#        {"ev": "output", "id": "<cell>", "output": {...nbformat output...}}
#        {"ev": "clear", "id": "<cell>"}
#        {"ev": "update", "id": "<cell>", "index": 0, "output": {...}}
#        {"ev": "done", "id": "<cell>", "execution_count": 3, "status": "ok"|"error"}
#        {"ev": "error", "message": "..."}
#
# argv: kernel name (default "python3"), working directory.

import json
import queue
import sys
import threading

out_lock = threading.Lock()


def emit(obj):
    with out_lock:
        sys.stdout.write(json.dumps(obj) + "\n")
        sys.stdout.flush()


try:
    from jupyter_client.manager import KernelManager
except Exception as e:  # noqa: BLE001 - report any import failure
    emit({"ev": "error", "message": f"jupyter_client is not importable: {e}"})
    sys.exit(1)

name = sys.argv[1] if len(sys.argv) > 1 and sys.argv[1] else "python3"
cwd = sys.argv[2] if len(sys.argv) > 2 else None

try:
    km = KernelManager(kernel_name=name)
    km.start_kernel(cwd=cwd)
    kc = km.client()
    kc.start_channels()
    kc.wait_for_ready(timeout=60)
except Exception as e:  # noqa: BLE001
    emit({"ev": "error", "message": f"kernel {name!r} did not start: {e}"})
    sys.exit(1)


def ready():
    try:
        display = km.kernel_spec.display_name
    except Exception:  # noqa: BLE001
        display = name
    emit({"ev": "ready", "kernel": name, "display": display})


# msg_id of an execute request -> [cell id, execution_count, status]. Only
# the kernel thread below touches it.
cells = {}
requests = queue.Queue()
# Updatable displays (#1475): cell id -> outputs it holds since its run began
# or was cleared, and display_id -> [(cell id, output index)] of each output
# a `display(..., display_id=...)` made, so an `update_display_data` from any
# cell can replace them in place. croft stores one output per output event,
# so the counts index its list.
counts = {}
displays = {}


def forget_outputs(cell_id):
    counts[cell_id] = 0
    for spots in displays.values():
        spots[:] = [s for s in spots if s[0] != cell_id]


def as_output(kind, c):
    if kind == "stream":
        return {"output_type": "stream", "name": c["name"], "text": c["text"]}
    if kind == "execute_result":
        return {
            "output_type": "execute_result",
            "execution_count": c.get("execution_count"),
            "data": c.get("data", {}),
            "metadata": c.get("metadata", {}),
        }
    if kind == "display_data":
        return {
            "output_type": "display_data",
            "data": c.get("data", {}),
            "metadata": c.get("metadata", {}),
        }
    if kind == "error":
        return {
            "output_type": "error",
            "ename": c.get("ename", ""),
            "evalue": c.get("evalue", ""),
            "traceback": c.get("traceback", []),
        }
    return None


def on_update(c):
    # Whichever cell sent it, it replaces the outputs that display made.
    display_id = c.get("transient", {}).get("display_id")
    out = {
        "output_type": "display_data",
        "data": c.get("data", {}),
        "metadata": c.get("metadata", {}),
    }
    for cell_id, index in displays.get(display_id, []):
        emit({"ev": "update", "id": cell_id, "index": index, "output": out})


def on_iopub(msg):
    kind = msg["msg_type"]
    c = msg["content"]
    if kind == "update_display_data":
        on_update(c)
        return
    parent = msg.get("parent_header", {}).get("msg_id")
    cell = cells.get(parent)
    if cell is None:
        return
    if kind == "execute_input":
        cell[1] = c.get("execution_count")
    elif kind == "clear_output":
        forget_outputs(cell[0])
        emit({"ev": "clear", "id": cell[0]})
    elif kind == "status" and c.get("execution_state") == "idle":
        cells.pop(parent, None)
        emit({"ev": "done", "id": cell[0], "execution_count": cell[1], "status": cell[2]})
    else:
        out = as_output(kind, c)
        if out is not None:
            if kind == "error":
                cell[2] = "error"
            index = counts.get(cell[0], 0)
            counts[cell[0]] = index + 1
            display_id = c.get("transient", {}).get("display_id")
            if display_id and kind in ("display_data", "execute_result"):
                displays.setdefault(display_id, []).append((cell[0], index))
            emit({"ev": "output", "id": cell[0], "output": out})


def settle_lost_cells():
    # The old kernel never answers what it was running or had queued, so no
    # idle status will settle those cells: settle them here, as failed, or
    # they show In [*] for good.
    lost = list(cells.values())
    cells.clear()
    for cell in lost:
        emit({"ev": "done", "id": cell[0], "execution_count": cell[1], "status": "error"})


def handle(req):
    op = req.get("op")
    try:
        if op == "execute":
            msg_id = kc.execute(req.get("code", ""), store_history=True)
            cells[msg_id] = [req.get("id", ""), None, "ok"]
        elif op == "interrupt":
            km.interrupt_kernel()
        elif op == "restart":
            try:
                km.restart_kernel(now=False)
                kc.wait_for_ready(timeout=60)
            finally:
                settle_lost_cells()
            ready()
    except Exception as e:  # noqa: BLE001 - keep serving after a failed op
        emit({"ev": "error", "message": f"{op}: {e}"})


def kernel_loop():
    # The one thread that talks to the kernel (#1477). zmq sockets are not
    # thread-safe, and `wait_for_ready` during a restart reads the shell and
    # iopub channels itself: with a second thread polling them, it lost the
    # reply or corrupted the frames, and the notebook hung at In [*].
    while True:
        try:
            req = requests.get_nowait()
        except queue.Empty:
            req = None
        if req is not None:
            if req.get("op") == "shutdown":
                return
            handle(req)
        # Only wait on iopub when nothing is queued: a Run All of many cells
        # behind a silent cell would otherwise pay the timeout per request,
        # holding back the rest and any Interrupt or Restart behind them.
        try:
            wait = 0 if not requests.empty() else 0.05
            on_iopub(kc.get_iopub_msg(timeout=wait))
        except queue.Empty:
            pass
        except Exception:  # noqa: BLE001 - channel closed on shutdown
            return
        # Execute replies are not needed (iopub carries everything), but
        # they must be read or they pile up for the kernel's lifetime.
        try:
            kc.get_shell_msg(timeout=0)
        except queue.Empty:
            pass
        except Exception:  # noqa: BLE001
            return


worker = threading.Thread(target=kernel_loop, daemon=True)
worker.start()
ready()

for line in sys.stdin:
    try:
        req = json.loads(line)
    except ValueError:
        continue
    requests.put(req)
    if req.get("op") == "shutdown":
        break
else:
    requests.put({"op": "shutdown"})

worker.join(timeout=70)
try:
    kc.stop_channels()
    km.shutdown_kernel(now=True)
except Exception:  # noqa: BLE001
    pass
