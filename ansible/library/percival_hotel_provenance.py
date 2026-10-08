#!/usr/bin/python
"""Verify the effective/running hotel artifact without changing systemd."""
from ansible.module_utils.basic import AnsibleModule
from ansible.module_utils.percival_provenance import systemd_properties, verify


def main():
    module = AnsibleModule(argument_spec={
        'approved_path': {'type': 'path', 'required': True},
        'approved_sha256': {'type': 'str', 'required': True},
        'require_running': {'type': 'bool', 'default': False},
    }, supports_check_mode=True)
    try:
        before = systemd_properties()
        result = verify(before, module.params['approved_path'],
                        module.params['approved_sha256'], module.params['require_running'])
        if systemd_properties() != before:
            raise ValueError('Hotel supervisor properties changed during verification')
    except Exception:
        # Do not expose ExecStart argv, process/environment content or exception data.
        module.fail_json(msg='Hotel executable provenance failed; gateway activation is blocked. Review the approved artifact and effective/running service paths locally.')
    module.exit_json(changed=False, **result)


if __name__ == '__main__':
    main()
