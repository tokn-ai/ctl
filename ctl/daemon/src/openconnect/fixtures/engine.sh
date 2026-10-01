#!/bin/sh
set -eu
root=${0%/*}
exec python3 - "$root" "$@" 3<&0 <<'PYCODE'
import json, os, pathlib, sys, time
root=pathlib.Path(sys.argv[1])
args=sys.argv[2:]
path=root/'container.json'
command=args[0]
if command=='run':
  (root/'run.args').write_text('\n'.join(args)+'\n')
  (root/'run.pid').write_text(str(os.getpid()))
  if (root/'failure').exists():
    sys.stderr.write((root/'failure').read_text())
    sys.exit(1)
  try: (root/'reservation').mkdir()
  except FileExistsError:
    (root/'name_conflict').touch()
    sys.exit(1)
  labels={}
  for i,arg in enumerate(args):
    if arg=='--label':
      key,value=args[i+1].split('=',1); labels[key]=value
  name=args[args.index('--name')+1]
  container={'Id':'a'*64,'Name':'/'+name,'Config':{'Labels':labels},'State':{'Running':True,'Status':'running','ExitCode':0,'Health':{'Status':'healthy'}},'NetworkSettings':{'Ports':{'1080/tcp':[{'HostIp':'127.0.0.1','HostPort':'49152'}]}}}
  if (root/'created').exists(): container['State'].update(Running=False,Status='created')
  if (root/'delayed_publication').exists():
    # Make the conflicting run exit before inspect can observe the winner.
    # Multiple probes force startup to tolerate a pending name reservation.
    while True:
      probes=root/'unpublished.inspect'
      if (root/'name_conflict').exists() and probes.exists() and len(probes.read_text().splitlines()) >= 3:
        break
      time.sleep(0.01)
  temporary=root/'container.pending'
  temporary.write_text(json.dumps([container]))
  temporary.replace(path)
  (root/'run.count').write_text('1')
  # This Python script consumes its source on stdin; fd3 retains docker stdin.
  with os.fdopen(3) as stream: (root/'input').write_text(stream.readline())
  while True: time.sleep(0.05)
elif command=='container' and args[1]=='inspect':
  if not path.exists():
    if (root/'name_conflict').exists():
      with (root/'unpublished.inspect').open('a') as stream: stream.write('missing\n')
    sys.stderr.write('Error: No such container\n'); sys.exit(1)
  data=json.loads(path.read_text())
  if (root/'exited').exists(): data[0]['State'].update(Running=False,Status='exited')
  print(json.dumps(data))
elif command=='ps':
  if path.exists(): print('a'*64)
elif command=='exec':
  with (root/'exec.calls').open('a') as stream: stream.write('\n'.join(args)+'\n')
  if args[-1]=='/run/ctl/heartbeat.sh':
    with (root/'heartbeats').open('a') as stream: stream.write('ping\n')
  elif not (root/'ready').exists(): sys.exit(1)
elif command=='rm':
  (root/'remove.args').write_text('\n'.join(args))
  path.unlink(missing_ok=True)
else: sys.exit(2)
PYCODE
