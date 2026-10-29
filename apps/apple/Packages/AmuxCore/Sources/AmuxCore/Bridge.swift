import AmuxApp
import AmuxValues
import Foundation

extension Bridge {
    /// The version of the Rust library this build linked. A library built
    /// with the driving tools says so after a `+`; a release binary is
    /// checked for not carrying that suffix, so it is spelled nowhere here.
    public static var version: String {
        guard let version = amux_version() else { return "" }
        return String(cString: version)
    }

    /// Why `name` cannot name an agent, or nil when it can: the rule every
    /// host holds a create or a rename to.
    public static func agentNameProblem(_ name: String) -> AgentNameProblem? {
        name.withCString { Bridge.read(AgentNameProblem.self, amux_agent_name_problem($0)) }
    }

    /// A new agent's settings from what its host offers and what is chosen
    /// so far, as the shared settings view builds them.
    public static func newAgentSettings(_ catalogue: Catalogue, _ chosen: NewAgentChoices) -> SettingsView? {
        Bridge.json(catalogue).withCString { catalogue in
            Bridge.json(chosen).withCString { chosen in
                Bridge.read(SettingsView.self, amux_new_agent_settings(catalogue, chosen))
            }
        }
    }

    /// What is chosen for a new agent after `pick`, with the rules a pick
    /// carries: another model starts at its default effort and gives up a
    /// permission it does not take.
    public static func newAgentPick(
        _ catalogue: Catalogue, _ chosen: NewAgentChoices, _ pick: NewAgentPick
    ) -> NewAgentChoices? {
        Bridge.json(catalogue).withCString { catalogue in
            Bridge.json(chosen).withCString { chosen in
                Bridge.json(pick).withCString { pick in
                    Bridge.read(NewAgentChoices.self, amux_new_agent_pick(catalogue, chosen, pick))
                }
            }
        }
    }

    /// Each field's problem with the values entered, one per field in the
    /// form's order, as the shared view checks a form; empty when it can be
    /// submitted.
    public static func formProblems(_ fields: [FormField], _ values: [FormValue]) -> [FormProblem]? {
        Bridge.json(fields).withCString { fields in
            Bridge.json(values).withCString { values in
                Bridge.read([FormProblem].self, amux_form_problems(fields, values))
            }
        }
    }

    /// The agent's commands matching the word `draft` ends in, as the shared
    /// view finds them, each with the draft it leaves once picked.
    public static func commandMatches(_ commands: [CommandView], _ draft: String) -> [CommandMatch] {
        Bridge.json(commands).withCString { commands in
            draft.withCString { draft in
                Bridge.read([CommandMatch].self, amux_command_matches(commands, draft)) ?? []
            }
        }
    }

    /// Whether text arriving in one paste joins the draft as one text
    /// attachment rather than as words, by the rule every client's paste
    /// follows.
    public static func pastesAsAttachment(_ text: String) -> Bool {
        text.withCString { amux_pastes_as_attachment($0) }
    }

    /// The most an attachment may hold, in bytes; the hosts refuse more.
    public static var attachmentLimit: Int { Int(amux_attachment_limit()) }
}
