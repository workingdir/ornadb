-- SecurityAdmin privileges are class-wide and therefore cannot name an object.
ALTER TABLE _orna_kernel.security_privilege_grants
    ADD CONSTRAINT security_privilege_grants_security_admin_class_wide_check
    CHECK (privilege_class <> 'security_admin' OR object_id = '');
