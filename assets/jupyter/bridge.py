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


# msg_id of an execute request -> [cell id, execution_count, status]
cells = {}
cells_lock = threading.Lock()
stopping = threading.Event()


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


def pump_iopub():
    while not stopping.is_set():
        try:
            msg = kc.get_iopub_msg(timeout=0.2)
        except queue.Empty:
            continue
        except Exception:  # noqa: BLE001 - channel closed on shutdown
            return
        parent = msg.get("parent_header", {}).get("msg_id")
        with cells_lock:
            cell = cells.get(parent)
        if cell is None:
            continue
        kind = msg["msg_type"]
        c = msg["content"]
        if kind == "execute_input":
            cell[1] = c.get("execution_count")
        elif kind == "clear_output":
            emit({"ev": "clear", "id": cell[0]})
        elif kind == "status" and c.get("execution_state") == "idle":
            with cells_lock:
                cells.pop(parent, None)
            emit({"ev": "done", "id": cell[0], "execution_count": cell[1], "status": cell[2]})
        else:
            out = as_output(kind, c)
            if out is not None:
                if kind == "error":
                    cell[2] = "error"
                emit({"ev": "output", "id": cell[0], "output": out})


def drain_shell():
    # Execute replies are not needed (iopub carries everything), but they
    # must be read or they pile up for the kernel's lifetime.
    while not stopping.is_set():
        try:
            kc.get_shell_msg(timeout=0.2)
        except queue.Empty:
            continue
        except Exception:  # noqa: BLE001
            return


threading.Thread(target=pump_iopub, daemon=True).start()
threading.Thread(target=drain_shell, daemon=True).start()
ready()

for line in sys.stdin:
    try:
        req = json.loads(line)
    except ValueError:
        continue
    op = req.get("op")
    try:
        if op == "execute":
            with cells_lock:
                msg_id = kc.execute(req.get("code", ""), store_history=True)
                cells[msg_id] = [req.get("id", ""), None, "ok"]
        elif op == "interrupt":
            km.interrupt_kernel()
        elif op == "restart":
            km.restart_kernel(now=False)
            kc.wait_for_ready(timeout=60)
            ready()
        elif op == "shutdown":
            break
    except Exception as e:  # noqa: BLE001 - keep serving after a failed op
        emit({"ev": "error", "message": f"{op}: {e}"})

stopping.set()
try:
    kc.stop_channels()
    km.shutdown_kernel(now=True)
except Exception:  # noqa: BLE001
    pass
