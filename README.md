# lusid

_STATUS: MAD SCIENCE 🧪_

![frankenstein](https://github.com/user-attachments/assets/53b049ef-a256-4b41-9e01-240660fb0153)

> Use declarative code to configure your living computer.

## About

Lusid helps you configure your computers with the exact setup you describe.

Like .dotfiles on steroids, but less "pure" (ideological) than NixOS. Like Ansible or Salt Stack, but more friendly and functional for personal setups.

Lusid can be used for your workstations (desktops or laptops) or your servers (homelab or cloud).

## Get Started

### Install

TODO

### Create a plan

TODO

### Apply a plan

TODO

## Concepts

### Plan

A plan describes a modular set of resources you want to be applied to the machine.

Plans are written in the [the Rimu language](https://rimu.dev):

```
name: "example-git-setup"
version: "0.1.0"

params:
  whatever:
    type: "boolean"

setup: (params, system) =>
  - module: "@core/file"
    params:
      type: "source"
      source: "./gitconfig"
      path: system.user.home + ".gitconfig"

  - module: "@core/apt"
    params:
      packages: ["git"]

  - module: "@core/command"
    params:
      status: "install"
      install: "sudo apt-get install -y ripgrep"
      is_installed: "command -v rg"
```

A plan:

- Defines basic metadata like name and version (e.g. think `package.json` or `Cargo.toml`)
- Defines parameters that it expects to receive
- Defines a `setup` function, which return a list of items to apply.
  - An item can refer to another plan defined by the user, in which case they are called.
  - Or, an item can a core states, these are defined in Rust and called like any other plan.
- Items can be dependent: there is a way to say this _requires_ or is _required_by_ another item.

When a plan is applied:

- Given the inputs, the outputs should construct a tree.
  - The branches are user modules, the leaves are core states.
- The core states are evaluated from user-facing params into a sub-tree of atomic resources.
- For each resource, find the current state of the resource on your computer, then compare with the desired state to determine a resource change.
- Convert each resource change into a sub-tree of operations.
- From the causality tree, find a minimal list of ordered epochs, where each epoch is a list of operations that can be applied together.
- Merge all operations of the same type in the same epoch.
- Iterate through each epoch in order, applying the operations.

### Resource

A resource represents the intended state of a thing on your computer, e.g. a package or a file or a service.

Resource types:

- [x] [Apt](./resource/src/resources/apt.rs)
    - [ ] Repository ([TODO](https://github.com/ahdinosaur/lusid/issues/24))
- [ ] Command ([TODO](https://github.com/ahdinosaur/lusid/issues/30))
- [x] [File](./resource/src/resources/file.rs)
- [ ] FlatPak ([TODO](https://github.com/ahdinosaur/lusid/issues/32))
- [ ] Git ([TODO](https://github.com/ahdinosaur/lusid/issues/33))
- [ ] Group ([TODO](https://github.com/ahdinosaur/lusid/issues/29))
- [x] [Pacman](./resource/src/resources/pacman.rs)
- [ ] Systemd Service ([TODO](https://github.com/ahdinosaur/lusid/issues/27))
- [ ] User ([TODO](https://github.com/ahdinosaur/lusid/issues/28))

Each resource type defines:

- The user-facing parameters to describe such resources
- How to evaluate user-facing params into atomic resources: each atomic resource representing one thing on your computer.
- How to find the current state of the resource on your computer.
- Given the current state and the desired state, what change should be applied?
- How to apply the change as a set of operations.

### Operation

An operation is an action you can apply to your computer, e.g. installing a package, writing a file, or reloading a service.

Operation types:

- [x] [Apt](./operation/src/operations/apt.rs)
    - [ ] Repository ([TODO](https://github.com/ahdinosaur/lusid/issues/24))
- [ ] Command ([TODO](https://github.com/ahdinosaur/lusid/issues/30))
- [x] [File](./operation/src/operations/file.rs)
- [ ] FlatPak ([TODO](https://github.com/ahdinosaur/lusid/issues/32))
- [ ] Git ([TODO](https://github.com/ahdinosaur/lusid/issues/33))
- [ ] Group ([TODO](https://github.com/ahdinosaur/lusid/issues/29))
- [x] [Pacman](./operation/src/operations/pacman.rs)
- [ ] Systemd Service ([TODO](https://github.com/ahdinosaur/lusid/issues/27))
- [ ] User ([TODO](https://github.com/ahdinosaur/lusid/issues/28))

Each operation type defines:

- How to merge multiple operations of the same type
- How to apply an operation

## Roadmap

- [ ] Implement my complete personal "SnugOS" config
- [ ] Add system (i.e. Salt Stack "grains"): https://github.com/ahdinosaur/lusid/issues/9
- [ ] Add secrets management: https://github.com/ahdinosaur/lusid/issues/7
- [ ] Add Nix-like immutable package builder: https://github.com/ahdinosaur/lusid/issues/1
- [ ] Add unit testing framework for plans: https://github.com/ahdinosaur/lusid/issues/11
- [ ] Add install hooks: https://github.com/ahdinosaur/lusid/issues/31

## Related projects

- [comtrya](https://github.com/comtrya/comtrya)
- (legacy) [boxen](https://github.com/boxen/boxen)
