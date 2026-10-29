# cw join against a head with sign-in on (enforced mode)

Isolated head on 127.0.0.1:7860 with `CW_AUTH=required`, one administrator, own data dir. Script: scratchpad `enforced.sh` (steps listed in the output). Joining PC is a second process with its own empty data dir (`CW_HOME`).

```
mode: auth: no administrator yet
{"ok":true,"user":{"name":"admin","role":"administrator"}}
anon make code:   401
anon install.sh:  401
anon enroll:      401
anon join page:   200
admin made code BSRG-6NEG
install.sh with code: 200
--- first join output
cw  [2/5] contacting http://127.0.0.1:7860
cw  [3/5] enrolling
cw  [4/5] fetching models and settings from the head
cw  vision/models/yolo26s.onnx: 10%
cw  vision/models/yolo26s.onnx: 20%
cw  vision/models/yolo26s.onnx: 30%
cw  vision/models/yolo26s.onnx: 40%
cw  vision/models/yolo26s.onnx: 50%
cw  vision/models/yolo26s.onnx: 60%
cw  vision/models/yolo26s.onnx: 70%
cw  vision/models/yolo26s.onnx: 80%
cw  vision/models/yolo26s.onnx: 90%
--- nodes (admin):
[('e1', 'enf1', 'idle')]
--- second PC, same code:
cw  [2/5] contacting http://127.0.0.1:7860
cw  [3/5] enrolling
cw: the head refused to enroll this PC (HTTP 401): that pairing code is wrong, used up or expired; make a new one on the /join page
--- wrong code:
cw  [3/5] enrolling
cw: the head refused to enroll this PC (HTTP 401): that pairing code is wrong, used up or expired; make a new one on the /join page
--- worker route without token: 401 ; cluster without session: 401
--- auth.json holds hashes only:
[('enf1', 64)] raw token stored: False
```
