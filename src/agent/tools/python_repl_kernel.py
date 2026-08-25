import ast
import asyncio
import inspect
import json
import os
import socket
import sys
import traceback

PROTOCOL_VERSION = 1


def send_frame(stream, value):
    body = json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    stream.write(f"Content-Length: {len(body)}\r\n\r\n".encode("ascii"))
    stream.write(body)
    stream.flush()


def read_frame(stream):
    headers = {}
    while True:
        line = stream.readline()
        if not line:
            return None
        if line == b"\r\n":
            break
        name, separator, value = line.decode("ascii").partition(":")
        if not separator:
            raise ValueError("malformed frame header")
        headers[name.lower()] = value.strip()
    length = int(headers["content-length"])
    body = stream.read(length)
    if len(body) != length:
        raise EOFError("incomplete frame")
    return json.loads(body.decode("utf-8"))


def evaluate(code, namespace):
    module = ast.parse(code, mode="exec")
    result = None
    if module.body and isinstance(module.body[-1], ast.Expr):
        leading = ast.Module(body=module.body[:-1], type_ignores=[])
        if leading.body:
            compiled = compile(
                leading,
                "<python_repl_execute code>",
                "exec",
                flags=ast.PyCF_ALLOW_TOP_LEVEL_AWAIT,
            )
            pending = eval(compiled, namespace, namespace)
            if inspect.isawaitable(pending):
                asyncio.run(pending)
        expression = ast.Expression(module.body[-1].value)
        compiled = compile(
            expression,
            "<python_repl_execute code>",
            "eval",
            flags=ast.PyCF_ALLOW_TOP_LEVEL_AWAIT,
        )
        result = eval(compiled, namespace, namespace)
        if inspect.isawaitable(result):
            result = asyncio.run(result)
    else:
        compiled = compile(
            module,
            "<python_repl_execute code>",
            "exec",
            flags=ast.PyCF_ALLOW_TOP_LEVEL_AWAIT,
        )
        pending = eval(compiled, namespace, namespace)
        if inspect.isawaitable(pending):
            asyncio.run(pending)
    return None if result is None else repr(result)


def main():
    address = ("127.0.0.1", int(os.environ["XI_REPL_PORT"]))
    token = os.environ["XI_REPL_TOKEN"]
    sock = socket.create_connection(address, timeout=10)
    sock.settimeout(None)
    stream = sock.makefile("rwb", buffering=0)
    send_frame(
        stream,
        {
            "token": token,
            "protocol_version": PROTOCOL_VERSION,
            "runtime": "python",
            "runtime_version": sys.version.split()[0],
        },
    )
    namespace = {"__name__": "__repl__", "__builtins__": __builtins__}
    while True:
        request = read_frame(stream)
        if request is None:
            return
        request_id = request.get("id")
        method = request.get("method")
        if method == "shutdown":
            send_frame(stream, {"id": request_id, "result": None, "exception": None})
            return
        if method != "execute":
            send_frame(
                stream,
                {"id": request_id, "result": None, "exception": f"unknown method: {method}"},
            )
            continue
        try:
            result = evaluate(request.get("code", ""), namespace)
            response = {"id": request_id, "result": result, "exception": None}
        except SystemExit as error:
            sys.stdout.flush()
            sys.stderr.flush()
            code = error.code if isinstance(error.code, int) else (0 if error.code is None else 1)
            os._exit(code)
        except BaseException:
            response = {
                "id": request_id,
                "result": None,
                "exception": traceback.format_exc(),
            }
        sys.stdout.flush()
        sys.stderr.flush()
        send_frame(stream, response)


if __name__ == "__main__":
    main()
