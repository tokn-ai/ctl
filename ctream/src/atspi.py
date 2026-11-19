"""Resolve accessibility objects within one Sway window before dispatch."""
import glob
import json
import os
import subprocess
import sys

import gi
gi.require_version("Atspi", "2.0")
from gi.repository import Atspi, GLib


def descendants(node):
  pending = [node]
  for _ in range(2048):
    if not pending:
      return
    node = pending.pop()
    yield node
    count = node.get_child_count()
    if count > 2048:
      raise ValueError("Accessibility subtree exceeds the element limit")
    for index in reversed(range(count)):
      child = node.get_child_at_index(index)
      if child is not None:
        pending.append(child)
    if len(pending) > 2048:
      raise ValueError("Accessibility subtree exceeds the element limit")
  if pending:
    raise ValueError("Accessibility subtree exceeds the element limit")


def window_pid(window_id, title):
  matches = []
  for socket in glob.glob(os.path.join(os.environ["XDG_RUNTIME_DIR"], "sway-ipc.*.sock")):
    tree = json.loads(subprocess.check_output(
        ["swaymsg", "-s", socket, "-t", "get_tree", "-r"], timeout=3))
    pending = [tree]
    while pending:
      node = pending.pop()
      if node.get("foreign_toplevel_identifier") == window_id and node.get("name") == title:
        matches.append(node.get("pid"))
      pending.extend(node.get("nodes", []))
      pending.extend(node.get("floating_nodes", []))
  if len(matches) != 1 or not matches[0]:
    raise ValueError("Could not uniquely map the selected Sway window to an application")
  return matches[0]


def accessible_root(pid, title):
  desktop = Atspi.get_desktop(0)
  matches = []
  for index in range(desktop.get_child_count()):
    app = desktop.get_child_at_index(index)
    if app is None or app.get_process_id() != pid:
      continue
    for index in range(app.get_child_count()):
      window = app.get_child_at_index(index)
      if window and window.get_name() == title and window.get_role() in (
          Atspi.Role.FRAME, Atspi.Role.DIALOG, Atspi.Role.WINDOW):
        matches.append(window)
  if len(matches) != 1:
    raise ValueError("No unique accessible window for the selected application and title")
  return matches[0]


def action_names(node):
  if not node.is_action():
    return []
  return [node.get_action_name(index) for index in range(node.get_n_actions())]


def visible_extents(node):
  try:
    if not node.is_component():
      return None
    states = node.get_state_set()
    if states is None or not (
        states.contains(Atspi.StateType.VISIBLE) and states.contains(Atspi.StateType.SHOWING)):
      return None
    rect = node.get_extents(Atspi.CoordType.WINDOW)
  except GLib.Error:
    # An object can disappear, or its provider may reject this coordinate type.
    return None
  if rect is None or rect.width <= 0 or rect.height <= 0:
    return None
  if rect.x == -(2**31) or rect.y == -(2**31):
    return None
  return rect.x, rect.y, rect.width, rect.height


def relative_bounds(window, control):
  window_x, window_y, window_width, window_height = window
  x, y, width, height = control
  left = max(x - window_x, 0)
  top = max(y - window_y, 0)
  right = min(x - window_x + width, window_width)
  bottom = min(y - window_y + height, window_height)
  if left >= right or top >= bottom:
    return None
  return dict(x=left / window_width, y=top / window_height,
              width=(right - left) / window_width, height=(bottom - top) / window_height)


def run(operation, window_id, title):
  Atspi.init()
  Atspi.set_timeout(2000, 3000)
  root = accessible_root(window_pid(window_id, title), title)
  if operation in ("elements", "elements-with-bounds"):
    # WINDOW coordinates keep this independent of the window's screen position.
    # Normalize against the accessible window so capture and viewer scales can differ.
    window = visible_extents(root) if operation == "elements-with-bounds" else None
    result = []
    for node in descendants(root):
      actions = action_names(node)
      editable = node.is_editable_text()
      if actions or editable:
        element = dict(path=node.path, name=node.get_name(), role=node.get_role_name(),
                       actions=actions, editable=editable)
        if window is not None:
          control = visible_extents(node)
          bounds = relative_bounds(window, control) if control is not None else None
          if bounds is not None:
            element["bounds"] = bounds
        result.append(element)
    if window is not None and visible_extents(root) != window:
      for element in result:
        element.pop("bounds", None)
    return result

  raw = sys.stdin.buffer.read(65537)
  if len(raw) > 65536:
    raise ValueError("Action exceeds the request limit")
  request = json.loads(raw)
  matches = [node for node in descendants(root) if node.path == request["path"]]
  if len(matches) != 1:
    raise ValueError("Element is unavailable in the selected window")
  node = matches[0]
  if request["operation"] == "invoke":
    index = request["index"]
    names = action_names(node)
    if not 0 <= index < len(names) or names[index] != request["name"]:
      raise ValueError("Selected action is no longer available")
    applied = node.do_action(index)
  elif request["operation"] == "set_text":
    if not node.is_editable_text():
      raise ValueError("Element no longer supports text editing")
    applied = node.set_text_contents(request["text"])
  else:
    raise ValueError("Unknown action")
  return dict(applied=bool(applied), error=None if applied else "Application rejected the action")


operation, window_id, title = sys.argv[1:]
try:
  print(json.dumps(run(operation, window_id, title)))
except Exception as error:
  if operation == "action":
    print(json.dumps(dict(applied=False, error=str(error))))
  else:
    sys.exit(str(error))
