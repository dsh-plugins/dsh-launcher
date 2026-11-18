import type { App } from 'vue'
import {
  Alert,
  Badge,
  Button,
  Checkbox,
  CheckboxGroup,
  Collapse,
  CollapseItem,
  Empty,
  Form,
  FormItem,
  Input,
  InputGroup,
  InputNumber,
  InputPassword,
  InputSearch,
  Layout,
  LayoutContent,
  LayoutHeader,
  Link,
  List,
  ListItem,
  Menu,
  MenuItem,
  Modal,
  Option,
  PageHeader,
  Pagination,
  Popconfirm,
  Progress,
  Radio,
  RadioGroup,
  Result,
  Scrollbar,
  Select,
  Space,
  Spin,
  Step,
  Steps,
  Switch,
  Table,
  TableColumn,
  Tag,
  Textarea,
  Tooltip,
} from '@dsh-plugin/dsh-ui-components-vue'

/** 启动器使用的控件统一由 DSH Vue 组件库提供。 */
export function installUi(app: App): void {
  app.component('DshAlert', Alert)
  app.component('DshBadge', Badge)
  app.component('DshButton', Button)
  app.component('DshCheckbox', Checkbox)
  app.component('DshCheckboxGroup', CheckboxGroup)
  app.component('DshCollapse', Collapse)
  app.component('DshCollapseItem', CollapseItem)
  app.component('DshEmpty', Empty)
  app.component('DshForm', Form)
  app.component('DshFormItem', FormItem)
  app.component('DshInput', Input)
  app.component('DshInputGroup', InputGroup)
  app.component('DshInputNumber', InputNumber)
  app.component('DshInputPassword', InputPassword)
  app.component('DshInputSearch', InputSearch)
  app.component('DshLayout', Layout)
  app.component('DshLayoutContent', LayoutContent)
  app.component('DshLayoutHeader', LayoutHeader)
  app.component('DshLink', Link)
  app.component('DshList', List)
  app.component('DshListItem', ListItem)
  app.component('DshMenu', Menu)
  app.component('DshMenuItem', MenuItem)
  app.component('DshModal', Modal)
  app.component('DshOption', Option)
  app.component('DshPageHeader', PageHeader)
  app.component('DshPagination', Pagination)
  app.component('DshPopconfirm', Popconfirm)
  app.component('DshProgress', Progress)
  app.component('DshRadio', Radio)
  app.component('DshRadioGroup', RadioGroup)
  app.component('DshResult', Result)
  app.component('DshScrollbar', Scrollbar)
  app.component('DshSelect', Select)
  app.component('DshSpace', Space)
  app.component('DshSpin', Spin)
  app.component('DshStep', Step)
  app.component('DshSteps', Steps)
  app.component('DshSwitch', Switch)
  app.component('DshTable', Table)
  app.component('DshTableColumn', TableColumn)
  app.component('DshTag', Tag)
  app.component('DshTextarea', Textarea)
  app.component('DshTooltip', Tooltip)
}

declare module 'vue' {
  interface GlobalComponents {
    DshAlert: typeof Alert
    DshBadge: typeof Badge
    DshButton: typeof Button
    DshCheckbox: typeof Checkbox
    DshCheckboxGroup: typeof CheckboxGroup
    DshCollapse: typeof Collapse
    DshCollapseItem: typeof CollapseItem
    DshEmpty: typeof Empty
    DshForm: typeof Form
    DshFormItem: typeof FormItem
    DshInput: typeof Input
    DshInputGroup: typeof InputGroup
    DshInputNumber: typeof InputNumber
    DshInputPassword: typeof InputPassword
    DshInputSearch: typeof InputSearch
    DshLayout: typeof Layout
    DshLayoutContent: typeof LayoutContent
    DshLayoutHeader: typeof LayoutHeader
    DshLink: typeof Link
    DshList: typeof List
    DshListItem: typeof ListItem
    DshMenu: typeof Menu
    DshMenuItem: typeof MenuItem
    DshModal: typeof Modal
    DshOption: typeof Option
    DshPageHeader: typeof PageHeader
    DshPagination: typeof Pagination
    DshPopconfirm: typeof Popconfirm
    DshProgress: typeof Progress
    DshRadio: typeof Radio
    DshRadioGroup: typeof RadioGroup
    DshResult: typeof Result
    DshScrollbar: typeof Scrollbar
    DshSelect: typeof Select
    DshSpace: typeof Space
    DshSpin: typeof Spin
    DshStep: typeof Step
    DshSteps: typeof Steps
    DshSwitch: typeof Switch
    DshTable: typeof Table
    DshTableColumn: typeof TableColumn
    DshTag: typeof Tag
    DshTextarea: typeof Textarea
    DshTooltip: typeof Tooltip
  }
}
