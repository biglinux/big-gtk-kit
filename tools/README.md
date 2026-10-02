# Ferramentas de vazamento do big-gtk-kit

Verificações automáticas de vazamento de memória para programas GTK4, nascidas da caça de
2026-09-26 no desktop BigLinux. Três peças, da mais barata à mais precisa; cada uma responde o
que a anterior não consegue. O verificador estático lê só Rust; a varredura, o censo e o
`reftrace` valem para qualquer processo GLib (Rust, C, Python, Vala).

| Peça | Pergunta | Precisa de tela |
|---|---|---|
| `static/leak_lint.py` | o código tem um formato que já vazou? | não |
| `sweep/leak_sweep.py` + `build/libgobject-census.so` | abrir e fechar esta tela deixa objetos de que tipo? | sim (sessão isolada com AT-SPI) |
| `build/reftrace.so` + `reftrace/reftrace.py` | quem segura o objeto que sobrou? | sim |

Requisitos: Python ≥ 3.11 (só biblioteca padrão; o driver usa `gi` com Atspi), GLib/GObject e um
compilador C. `make` constrói as duas bibliotecas; `make check` roda o teste de controle do
censo e os testes das ferramentas em Python; `make check-gtk` roda o censo por sinal num programa
GTK de verdade, dentro de `headless-gate.sh` (precisa de `kwin_wayland`).

`headless-gate.sh [--accessible] COMANDO...` roda o comando numa sessão KWin virtual e privada,
com barramento D-Bus próprio; `--accessible` sobe também o barramento de acessibilidade e um
HOME descartável, que é o que a varredura precisa.

## 1. Verificador estático

    static/leak_lint.py [--json] [--no-icons] [--errors-only] CAMINHO...

Erros (saída 1): `unparent()` dentro do `closed` do próprio popover, fechar janela detectando
`destroy`, `set_parent` sem `unparent`, `teardown` que lê o filho do item de lista, ícone com
imagem embutida (nos temas instalados na máquina que roda o verificador). Avisos: handler que segura o próprio widget ou um ancestral (também através de
uma closure guardada em `Rc`, de um filho posto por builder ou de um controlador), irmãos que se
seguram mutuamente, listas de seguidores podadas tarde, handler num objeto que vive o processo
inteiro (área de transferência do display, `VolumeMonitor`, a aplicação vista de um componente)
com o id descartado. Lê só arquivos `.rs`; código de teste é ignorado e um arquivo ilegível é
pulado com aviso.

Dispensa, sempre com motivo (sem motivo é erro):

    // leak-guard: allow(set-parent-without-unparent) o pai é um GtkOverlay, que desfaz no dispose

## 2. Varredura por tipo

    headless-gate.sh --accessible python3 sweep/leak_sweep.py SUPERFICIES.toml \
        [--only NOME...] [--quick] [--cycles N] [--json SAIDA] [--library CAMINHO]

O driver inicia o programa com o censo pré-carregado, espera o app no barramento de
acessibilidade e, para cada superfície: aquece, tira o censo A, abre e fecha N vezes, tira o
censo B e confirma com uma segunda rodada. Um tipo que ganhou ≥ 1 instância por ciclo é
vazamento, salvo `[[allow]]` com teto por ciclo e motivo (no arquivo ou na superfície). Uma
superfície que não fecha de verdade é relatada como tal. Saída 0 limpo, 1 vazamento, 2 não rodou
(arquivo incompleto, programa ou `gdbus` ausente); `LEAK_SWEEP_DIR` escolhe onde ficam censos e o
log do programa.

`command` tem de executar o próprio programa GTK: o driver pede o censo por sinal ao processo que
iniciou, e um script intermediário receberia o sinal no lugar dele. `{workdir}` é o único
marcador substituído; as sementes de `[program.files]` ficam sempre dentro dele.

Formato do arquivo de superfícies (exemplos completos no bigdesktop, em `scripts/leak-surfaces/`):

```toml
cycles = 6

[program]
command = ["target/debug/app", "{workdir}/pasta"]   # {workdir}: diretório descartável
atspi_name = "app"
dbus_name = "org.exemplo.App"
window_path = "/org/exemplo/App/window/1"

[program.env]
XDG_CONFIG_HOME = "{workdir}/config"

[program.files]                                     # sementes dentro de {workdir}
"pasta/nota.txt" = "um\n"

[[allow]]
type = "AdwBreakpoint"
per_cycle = 3
reason = "libadwaita 1.9.3: AdwBreakpointBin nunca solta seus breakpoints"

[[surface]]
name = "preferências"
quick = true
open = [{ do = "action", name = "preferences", settle = 1.0 }]
close = [{ do = "dismiss" }]
```

Passos: `action` (org.gtk.Actions.Activate), `dbus`, `command`, `toggle`/`click` (por nome
acessível), `dismiss` (Cancelar/Fechar em diálogos e janelas além da primeira), `close-new`
(o mesmo, só nas janelas que o ciclo abriu: para um shell que hospeda os apps), `wait`.

O censo serve para qualquer processo GLib, sem o driver:

    GOBJECT_CENSUS_OUT=DIR/c LD_PRELOAD=build/libgobject-census.so programa
    kill -USR2 PID        # escreve DIR/c.PID.1, .2, ...

Cada linha é `contagem Tipo` (e `contagem texture LxA`). A contagem é aproximada: só as
diferenças entre dois censos do mesmo processo significam algo. Só páginas residentes são lidas:
um objeto que foi para a swap não é contado. Como conta: o GLib de
distribuição não é construído com `G_ENABLE_DEBUG`, então `g_type_get_instance_count` dá 0; o
censo varre a memória anônima do próprio processo procurando ponteiros de classe, e só conta um
quando há contagem de referências plausível e, com o malloc da glibc, um bloco vivo do tamanho
exato de dados privados + instância começando no lugar certo. Com outro alocador (jemalloc,
tcmalloc, mimalloc, ASan) esse último teste não existe: o censo começa com a linha
`# malloc layout unknown`, e cópias do cabeçalho de um objeto contam como objetos. Diferenças
continuam valendo; a contagem absoluta fica maior.

## 3. Quem segura

    LD_PRELOAD=build/reftrace.so REFTRACE_TYPE=AdwDialog REFTRACE_OUT=DIR/rt programa
    reftrace/reftrace.py DIR/rt.PID BINARIO --objects        # vivos e liberados
    reftrace/reftrace.py DIR/rt.PID BINARIO --object N       # sítios que tomaram mais que soltaram

Cada evento é `N|R|S|s|U endereço contagem quadros…`, com a contagem de ANTES da operação: `N` é
a criação (`g_object_new` chamado de fora do libgobject), `s` o afundamento da referência
flutuante (assume a referência, não soma), `U 1` é a última liberação. Um sobrevivente cujas referências rastreadas se equilibram é segurado por
quem nunca chamou `g_object_ref` nele: a lista de janelas do GTK (janela nunca destruída) ou um
valor Rust nunca descartado.

## Vazamentos conhecidos das bibliotecas

Medidos em 2026-09-26 e 2026-10-02, no GTK 4.22.4, libadwaita 1.9.3 e GtkSourceView 5.20.0; o que o kit
contorna está na coluna da direita, e o que sobra vira um `[[allow]]` no arquivo de superfícies de quem o encontra.
Toda exceção leva teto por ciclo (por exemplo `AdwBreakpoint ≤ 3 por ciclo`): exceção sem teto
esconde regressão.

| Tipo | Crescimento | Origem | No kit |
|---|---|---|---|
| `AdwBreakpoint` | 1 por breakpoint do programa, por janela/diálogo fechado | libadwaita 1.9.3 `adw_breakpoint_bin_init` (array sem função de liberação) | `breakpoint::add` |
| `AdwBreakpoint` | 3 por abertura de `AdwDialog` adaptativo | o mesmo, nos breakpoints que o diálogo cria para si | inalcançável: `[[allow]]` com teto 3 por ciclo |
| a árvore inteira do diálogo | por diálogo fechado sobre janela sem foco | libadwaita 1.9 não devolve foco; GTK 4.22 perde a referência do foco adiado | `install()` (foco tirado no `closed`) |
| `AdwShortcutsSection`, itens, `GtkFilterListModel`, `GtkCustomFilter`, `AdwTimedAnimation` | 1 conjunto por abertura de `AdwShortcutsDialog` | o desmonte do diálogo | `dialog::present_kept` |
| `GdkWaylandToplevel` | 1 por janela que mostrou popover, menu ou tooltip | GDK 4.22.4 (e `main`): o popup Wayland toma o pai com `g_value_dup_object` e nunca solta | `install()` (solta ao finalizar o popup) |
| `GMenu` | 1 por seção mostrada, por menu construído | GTK 4.22 `gtk_menu_tracker_add_items` (seção não fundida) | `menu::append_shared_section` |
| `GSignalGroup` ×3, `GtkAdjustment` ×4 | por `GtkSourceView` com mapa | GtkSourceView 5.20: `GtkSourceCompletion` cria três grupos no init e nunca os libera | inalcançável |
| `GskVulkanImage`, `GtkCssStaticStyle`, `GtkIconPaintable`, `GskFillNode`/`GskColorNode` | poucos por abertura, estabiliza | caches do GTK | — (cache, não vazamento: memória estável em 20 aberturas) |

## Onde está em uso

- bigdesktop: `scripts/leak-gate.sh` roda o verificador, os censos do projeto e as varreduras
  (`--full` para todas as superfícies); `clippy.toml` bane as APIs que vazam.
- Harness: `~/.agents/skills/gtk-ui-ux/SKILL.md`,
  `~/.agents/skills/linux-ui-a11y/references/memory-leak-checking.md`,
  `~/.agents/templates/clippy/gtk4-libadwaita.toml`.
