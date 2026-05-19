======
Zoku
======

A minimalist terminal attach/detach tool.

Quick start
============

If you know `screen, tmux, zellij`, you already know how to use `zoku`. Create a new session named `demo`, run `bash` in it, and attach to it immediately:

.. code-block:: bash

    zoku new demo bash

This creates a Unix socket file named `demo` in current directory, which you can use to attach later:

.. code-block:: bash

    zoku attach demo

Upon attaching, zoku replays recent history in the session.

There is no escape key or super key to detach from a session. Just close your terminal window, or if you are inside a SSH connection, disconnect SSH with :kbd:`enter` and then :kbd:`shift + ~`.
