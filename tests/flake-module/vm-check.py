machine.wait_for_unit("multi-user.target")
machine.succeed("hello")
