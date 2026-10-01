#!/bin/sh
# A detached fake engine: container state and watchdog outlive each CLI invocation.
set -eu
root=$(dirname "$0")
exec python3 - "$root" "$@" <<'PY'
import json
import os
from pathlib import Path
import subprocess
import sys
import time

root = Path(sys.argv[1])
args = sys.argv[2:]
container = root / "container"
container_id = "a" * 64

def missing():
  print("Error: No such container", file=sys.stderr)
  sys.exit(1)

def append(name, value):
  with (root / name).open("a") as file:
    file.write(value + "\n")

if (root / "engine_unavailable").exists():
  print("Cannot connect to the container engine", file=sys.stderr)
  sys.exit(1)

if args[0] == "run":
  append("run.calls", "run")
  (root / "run.args").write_text("\n".join(args) + "\n")
  try:
    container.mkdir()
  except FileExistsError:
    sys.exit(1)
  append("created.calls", "created")
  labels = {}
  name = ""
  for index, value in enumerate(args[:-1]):
    if value == "--label":
      key, label = args[index + 1].split("=", 1)
      labels[key] = label
    if value == "--name":
      name = args[index + 1]
  (container / "labels").write_text(json.dumps(labels))
  (container / "name").write_text(name)
  if (root / "legacy").exists():
    (container / "labels").write_text(json.dumps({"io.ctl.lease": "legacy-owner"}))
  if (root / "created_only").exists():
    (container / "state").write_text("created")
  else:
    (container / "state").write_text("running")
    watchdog = r'''
import os
from pathlib import Path
import sys
import time
root = Path(sys.argv[1])
container = root / "container"
initial = time.monotonic()
while container.exists():
  last = initial
  try:
    last = float((container / "heartbeat").read_text())
  except (FileNotFoundError, ValueError):
    pass
  if time.monotonic() - last > 4:
    import shutil
    shutil.rmtree(container, ignore_errors=True)
    (root / "watchdog.exited").write_text("expired")
    break
  time.sleep(0.1)
'''
    child = subprocess.Popen([sys.executable, "-c", watchdog, str(root)],
      stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
      start_new_session=True)
    (root / "watchdog.pid").write_text(str(child.pid))
  print(container_id)
  if (root / "creator_client_failed").exists():
    sys.exit(1)

elif args[:2] == ["container", "inspect"]:
  append("inspect.calls", args[2])
  if not (container / "labels").exists():
    missing()
  name = (container / "name").read_text()
  if args[2] not in [container_id, name]:
    missing()
  state = (container / "state").read_text()
  print(json.dumps([{"Id": container_id, "Name": "/" + name,
    "Config": {"Labels": json.loads((container / "labels").read_text())},
    "State": {"Running": state == "running", "Status": state, "ExitCode": 0},
    "NetworkSettings": {"Ports": {"1080/tcp": [{"HostIp": "127.0.0.1", "HostPort": (root / "port").read_text()}]}}
  }]))

elif args[:2] == ["container", "ls"] or args[0] == "ps":
  if container.exists():
    print(container_id)

elif args[0] == "exec":
  if not container.exists() or (container / "state").read_text() != "running":
    missing()
  if args[1] != container_id:
    sys.exit(1)
  if "/run/ctl/heartbeat.sh" in args:
    value = str(time.monotonic())
    temporary = container / ("heartbeat-" + str(os.getpid()))
    temporary.write_text(value)
    temporary.replace(container / "heartbeat")
    append("heartbeats", value)
  else:
    if (root / "no_status").exists():
      sys.exit(1)
    print((root / "status.json").read_text())

elif args[0] == "rm":
  (root / "remove.args").write_text("\n".join(args) + "\n")
  if args[-1] != container_id:
    sys.exit(1)
  # Non-force removal is rejected if startup won the inspect/remove race.
  if container.exists() and (container / "state").read_text() == "running" and "--force" not in args:
    sys.exit(1)
  import shutil
  shutil.rmtree(container, ignore_errors=True)

elif args[0] == "stop":
  (root / "remove.args").write_text("\n".join(args) + "\n")
  import shutil
  shutil.rmtree(container, ignore_errors=True)

elif args[0] == "volume":
  if args[1] == "ls":
    if (root / "volume").exists():
      print((root / "volume").read_text(), end="")
  elif args[1] == "rm":
    (root / "volume-remove.args").write_text("\n".join(args) + "\n")
    if (root / "volume_in_use").exists() or container.exists():
      sys.exit(1)
    (root / "volume").unlink(missing_ok=True)
  else:
    sys.exit(1)
else:
  sys.exit(1)
PY
