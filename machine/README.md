# lusid-machine

Declarative description of a lusid *target* machine.

Distinct from `lusid-system`:

- `lusid-system::System` — facts about the machine lusid is running on *right now*.
- `lusid-machine::Machine` — the machine we want to provision (may be a VM,
  may be remote), described by its intended hostname / arch / OS plus optional
  `MachineVmOptions` (cpu, memory, graphics).

Currently a minimal data container; expected to grow as remote deployment,
credentials, and lifecycle policies are added.
