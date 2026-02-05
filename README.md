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
```

A plan:

- Defines basic metadata like name and version (e.g. think `package.json` or `Cargo.toml`)
- Defines parameters that it expects to receive
- Defines a `setup` function, which return a list of items to apply.
  - An item can refer to another plan defined by the user, in which case they are called.
  - Or, an item can a core states, these are defined in Rust and called like any other plan.
- Items can be dependent: there is a way to say this happens _before_ or _after_ this.

When a plan is applied:

- Given the inputs, the outputs should construct a tree.
  - The branches are user modules, the leaves are core states.
- The core states are evaluated from user-facing params into a sub-tree of atomic resources (each atomic resource representing one thing on your computer).
- For each resource, find the current state of the resource on your computer, then compare with the desired state to determine a resource change.
- Convert each resource change into a sub-tree of operations.
- From the causality tree, find a minimal list of ordered epochs, where each epoch is a list of operations that can be applied together.
- Merge all operations of the same type in the same epoch.
- Iterate through each epoch in order, applying the operations.

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
