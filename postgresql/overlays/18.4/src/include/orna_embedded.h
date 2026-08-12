/*-------------------------------------------------------------------------
 *
 * orna_embedded.h
 *    Private PostgreSQL 18 entry points for the Orna executable.
 *
 *-------------------------------------------------------------------------
 */
#ifndef ORNA_EMBEDDED_H
#define ORNA_EMBEDDED_H

#include <stdbool.h>

extern int orna_postgres18_entry(int argc, char *argv[]);
extern int orna_postgres18_initdb_entry(const char *data_directory);

extern int orna_postgres18_set_support_root(const char *absolute_root);
extern const char *orna_postgres18_support_root(void);
extern void orna_postgres18_set_system_functions_initialisation_capability(bool enabled);
extern bool orna_postgres18_has_system_functions_initialisation_capability(void);
extern void orna_postgres18_set_initialisation_child_capability(bool enabled);
extern bool orna_postgres18_has_initialisation_child_capability(void);
extern int orna_postgres18_install_exec_filter(void);

#endif							/* ORNA_EMBEDDED_H */
