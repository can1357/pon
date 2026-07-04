class style:
    INFO='\33[36m'
    RESET='\33[0m'
cmd = ['/Users/can/.cache/cargo-target/debug/pon-cli', '/tmp/x.py', 'setup']
print('join', ' '.join(cmd))
fmt = '{style.INFO}+ {cmd}{style.RESET}'
print(type(fmt.format), repr(fmt.format))
print(fmt.format(style=style, cmd=' '.join(cmd)))
